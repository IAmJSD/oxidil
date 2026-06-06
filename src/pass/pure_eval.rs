//! Pass: pure built-in & pure-expression evaluation ("GCC builtin folding"),
//! plus the conservative subset of constant evaluation that requires proving an
//! identifier is a *genuine global* (min level O1).
//!
//! This pass is the sibling of `constant_fold.rs`. The difference is ONE knob in
//! the evaluation context: `is_global_reference`.
//!
//!   - `constant_fold.rs` hard-codes `is_global_reference -> false`, so it never
//!     trusts `Math`/`String`/`Number`/`parseInt`/`.length` to be the real
//!     built-ins (they could be shadowed locals). It therefore only folds literal
//!     arithmetic / coercion that needs no global identity.
//!
//!   - `pure_eval.rs` answers `is_global_reference(ref)` by ASKING THE RESOLVED
//!     SCOPING: a reference is global iff it resolves to NO local symbol
//!     (`symbol_id().is_none()`). When that holds, the identifier is not shadowed
//!     by any binding in scope, so it IS the host global, and `oxc_ecmascript`'s
//!     hard-coded allow-list of pure built-ins (Math.{abs,floor,ceil,round,trunc,
//!     sign,min,max,sqrt,...}, Number()/String()/Boolean(), pure String.prototype
//!     methods on constant strings, String.fromCharCode, parseInt/parseFloat on
//!     constant strings, `.length` of constant string/array literals, etc.) may be
//!     evaluated and the result substituted.
//!
//! SOUNDNESS. The exact same `Ctx` (same `is_global_reference`) drives BOTH the
//! evaluator AND the `may_have_side_effects` guard, so:
//!   - a shadowed `Math` (resolved to a local symbol) is `is_global_reference ==
//!     false` -> neither folded nor considered pure -> left untouched;
//!   - `oxc_ecmascript` itself only folds operations on CONSTANT literal arguments
//!     and matches the spec exactly (NaN/-0/Infinity/float precision/i32 overflow);
//!   - we re-run the side-effect guard AFTER evaluating (the evaluator does NOT
//!     prove purity for sequence expressions, so `(f(),5)` must still be rejected);
//!   - we refuse to synthesize NaN/Infinity/-Infinity numeric literals, BigInt, and
//!     `undefined` (no safe literal token), exactly like `constant_fold.rs`.
//!
//! BAIL-OUT. `with` and direct `eval(...)` defeat scope resolution (a binding can
//! be introduced/observed under a name the analysis never reads), so
//! `is_global_reference` would be unreliable. We bail the whole pass on such
//! programs, matching DCE / rename.
//!
//! IDEMPOTENCE. We skip nodes that are already the literal kind we would emit, so
//! the driver fixpoint terminates.

use oxc_allocator::Allocator;
use oxc_ast::ast::{Expression, IdentifierReference, Program};
use oxc_ast::AstBuilder;
use oxc_ecmascript::constant_evaluation::{
    ConstantEvaluation, ConstantEvaluationCtx, ConstantValue,
};
use oxc_ecmascript::side_effects::{
    MayHaveSideEffects, MayHaveSideEffectsContext, PropertyReadSideEffects,
};
use oxc_ecmascript::GlobalContext;
use oxc_semantic::Scoping;
use oxc_span::SPAN;
use oxc_syntax::number::NumberBase;
use oxc_traverse::{Traverse, TraverseCtx};

use std::collections::HashSet;
use std::rc::Rc;

use crate::level::OptLevel;
use crate::pass::{run_traverse, Pass, PassConfig, PassResult};
use crate::semantic_util::{collect_patched_global_names, program_has_with_or_eval, PatchResult};

#[derive(Default)]
pub struct PureEval;

impl Pass for PureEval {
    fn name(&self) -> &'static str {
        "pure-eval"
    }

    fn min_level(&self) -> OptLevel {
        OptLevel::O1
    }

    fn run<'a>(
        &mut self,
        program: &mut Program<'a>,
        scoping: &mut Scoping,
        allocator: &'a Allocator,
        _cfg: &PassConfig,
    ) -> PassResult {
        // `with` / direct `eval` make `is_global_reference` unreliable: a name we
        // believe is the global `Math` might be rebound. Bail entirely.
        if program_has_with_or_eval(program) {
            return PassResult::UNCHANGED;
        }

        // MONKEY-PATCH GUARD (GRANULAR). `is_global_reference` is scope-only: it
        // cannot see that `Math.floor = ...` (or `Math = ...`) replaced a built-in
        // while the name still resolves to no local symbol. Folding `Math.floor(x)`
        // to the pristine spec result would then drop the patch's side effects and
        // emit a different value.
        //
        // Instead of bailing the WHOLE pass on any global write, we compute the set
        // of global root NAMES that are actually patched. A builtin call/member
        // whose root global name is NOT in that set is still pristine and may fold.
        // The one case we cannot name — a computed non-literal-key write on a global
        // root (`globalThis[k] = v`), which can redefine an arbitrarily-named
        // global — degrades to the old whole-pass bail (`PatchResult::All`).
        let patched: Rc<HashSet<String>> = match collect_patched_global_names(program, scoping) {
            PatchResult::All => return PassResult::UNCHANGED,
            PatchResult::Names(names) => Rc::new(names),
        };

        let mut visitor = PureEvalVisitor {
            changed: false,
            patched,
        };
        run_traverse(&mut visitor, allocator, program, scoping, ());
        if visitor.changed {
            PassResult::CHANGED
        } else {
            PassResult::UNCHANGED
        }
    }
}

struct PureEvalVisitor {
    changed: bool,
    /// Global root names that are monkey-patched somewhere in the program. A
    /// reference to one of these is treated as NON-global (not pristine), so its
    /// builtin folding is suppressed; every other global still folds.
    patched: Rc<HashSet<String>>,
}

impl<'a> Traverse<'a, ()> for PureEvalVisitor {
    fn exit_expression(&mut self, node: &mut Expression<'a>, ctx: &mut TraverseCtx<'a, ()>) {
        // Already a literal of the kind we emit? Skip to keep the fixpoint stable.
        if is_foldable_target_literal(node) {
            return;
        }

        // Build the evaluation context. `ctx.ast` is a `Copy` AstBuilder and
        // `ctx.scoping()` is an immutable borrow; both share `ctx` immutably.
        let eval_ctx = PureEvalCtx {
            ast: ctx.ast,
            scoping: ctx.scoping(),
            patched: &self.patched,
        };

        let Some(value) = node.evaluate_value(&eval_ctx) else {
            return;
        };

        // SOUNDNESS GUARD: `evaluate_value` does not prove purity (e.g. it folds a
        // `SequenceExpression` to its last operand while discarding the leading
        // operands' side effects). Refuse to replace anything still observable.
        // Computed while `node` is the ORIGINAL expression.
        if node.may_have_side_effects(&eval_ctx) {
            return;
        }

        let Some(replacement) = constant_to_expression(&value, ctx.ast) else {
            return;
        };

        *node = replacement;
        self.changed = true;
    }
}

/// True if `node` is already a literal of the kind our folder would emit, so
/// re-folding it would be a no-op churn. Keeps the driver fixpoint terminating.
fn is_foldable_target_literal(node: &Expression) -> bool {
    matches!(
        node,
        Expression::NumericLiteral(_)
            | Expression::BooleanLiteral(_)
            | Expression::StringLiteral(_)
            | Expression::NullLiteral(_)
    )
}

/// Rebuild a proven constant value as a fresh AST node in the arena.
/// Returns `None` for values we deliberately do not synthesize (non-finite
/// numbers, BigInt, Undefined) — same exclusions as `constant_fold.rs`.
fn constant_to_expression<'a>(
    value: &ConstantValue<'a>,
    ast: AstBuilder<'a>,
) -> Option<Expression<'a>> {
    match value {
        ConstantValue::Number(n) => {
            // NaN / Infinity / -Infinity have no numeric-literal token; do not
            // synthesize them (an identifier-based emit would need global-ref
            // safety we are not providing here).
            if !n.is_finite() {
                return None;
            }
            Some(ast.expression_numeric_literal(SPAN, *n, None, NumberBase::Decimal))
        }
        ConstantValue::String(s) => {
            let owned: &'a str = ast.allocator.alloc_str(s);
            Some(ast.expression_string_literal(SPAN, owned, None))
        }
        ConstantValue::Boolean(b) => Some(ast.expression_boolean_literal(SPAN, *b)),
        ConstantValue::Null => Some(ast.expression_null_literal(SPAN)),
        ConstantValue::BigInt(_) | ConstantValue::Undefined => None,
    }
}

/// The evaluation / side-effect context for pure built-in folding.
///
/// Identical conservative knobs to `constant_fold.rs` EXCEPT `is_global_reference`,
/// which consults the resolved scoping: a reference is "global" (i.e. the real
/// host built-in, not a shadowing local) iff it resolves to no local symbol.
struct PureEvalCtx<'a, 's> {
    ast: AstBuilder<'a>,
    scoping: &'s Scoping,
    /// Names of monkey-patched globals (see `PureEvalVisitor::patched`).
    patched: &'s HashSet<String>,
}

impl<'a, 's> GlobalContext<'a> for PureEvalCtx<'a, 's> {
    fn is_global_reference(&self, reference: &IdentifierReference<'a>) -> bool {
        // A reference is a genuine PRISTINE global iff (a) it resolves to no local
        // symbol (not shadowed by any in-scope binding) AND (b) its name is not in
        // the patched-globals set. A patched global (e.g. `Math` after
        // `Math.floor = f`) is reported NON-global so its builtins are never folded
        // to the spec result, while unpatched globals (`parseInt`, `Number`, ...)
        // still fold. If we have no reference id at all (shouldn't happen for
        // resolved trees), be conservative and say "not global".
        match reference.reference_id.get() {
            Some(reference_id) => {
                self.scoping
                    .get_reference(reference_id)
                    .symbol_id()
                    .is_none()
                    && !self.patched.contains(reference.name.as_str())
            }
            None => false,
        }
    }
}

impl<'a, 's> MayHaveSideEffectsContext<'a> for PureEvalCtx<'a, 's> {
    fn annotations(&self) -> bool {
        false
    }

    fn manual_pure_functions(&self, _callee: &Expression) -> bool {
        false
    }

    fn property_read_side_effects(&self) -> PropertyReadSideEffects {
        // Assume property reads may have side effects (getters). Conservative.
        // The known-global-property allow-list (e.g. `Math.PI`, `"abc".length`)
        // overrides this only for cases `oxc_ecmascript` has proven safe.
        PropertyReadSideEffects::All
    }

    fn unknown_global_side_effects(&self) -> bool {
        true
    }
}

impl<'a, 's> ConstantEvaluationCtx<'a> for PureEvalCtx<'a, 's> {
    fn ast(&self) -> AstBuilder<'a> {
        self.ast
    }
}
