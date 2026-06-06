//! Pass #1: constant folding via `oxc_ecmascript::ConstantEvaluation` (min level O1).
//!
//! This is the FULLY IMPLEMENTED reference pass. It proves the trait + traversal +
//! mutation pattern that every later pass copies:
//!   - build a `Traverse` visitor holding a `changed` flag,
//!   - fold in `exit_expression` (post-order: children fold before parents),
//!   - rebuild folded constants as fresh AST nodes via `ctx.ast` (same arena),
//!   - bridge `Scoping` through `run_traverse`.
//!
//! SOUNDNESS: we only replace an expression when `oxc_ecmascript` can prove a
//! constant value AND that expression `may_have_side_effects` reports false.
//! The side-effect guard is ESSENTIAL: `ConstantEvaluation` evaluates a
//! `SequenceExpression` (comma operator) to the value of its LAST operand without
//! verifying the leading operands are pure, so without the guard `(f(), 5)` would
//! fold to `5` and silently drop the call to `f()`. NaN, Infinity, -0, string
//! coercion and BigInt are all handled by `oxc_ecmascript`'s evaluator; anything
//! it cannot prove (e.g. `x + 1`, `foo()`, BigInt mixing) yields `None` and is
//! left untouched. We additionally refuse to fold expressions that already ARE
//! literals (so the fixpoint terminates) and skip BigInt results (we do not
//! synthesize BigInt nodes).

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

use crate::level::OptLevel;
use crate::pass::{run_traverse, Pass, PassConfig, PassResult};

#[derive(Default)]
pub struct ConstantFolding;

impl Pass for ConstantFolding {
    fn name(&self) -> &'static str {
        "constant-folding"
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
        let mut visitor = FoldVisitor { changed: false };
        run_traverse(&mut visitor, allocator, program, scoping, ());
        if visitor.changed {
            PassResult::CHANGED
        } else {
            PassResult::UNCHANGED
        }
    }
}

struct FoldVisitor {
    changed: bool,
}

impl<'a> Traverse<'a, ()> for FoldVisitor {
    fn exit_expression(&mut self, node: &mut Expression<'a>, ctx: &mut TraverseCtx<'a, ()>) {
        // Already a literal we would re-produce identically? Skip to keep the
        // fixpoint stable (avoid re-folding `5` into `5` forever).
        if is_foldable_target_literal(node) {
            return;
        }

        let eval_ctx = FoldCtx { ast: ctx.ast };
        let Some(value) = node.evaluate_value(&eval_ctx) else {
            return;
        };

        // SOUNDNESS GUARD: `evaluate_value` does NOT prove purity. In particular a
        // `SequenceExpression` folds to its last operand's value while discarding
        // the leading operands' side effects. Refuse to replace anything that may
        // have observable side effects (computed while `node` is still original).
        if node.may_have_side_effects(&eval_ctx) {
            return;
        }

        // Build the replacement node, or bail for things we will not synthesize.
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
/// Returns `None` for values we deliberately do not synthesize (BigInt, Undefined).
fn constant_to_expression<'a>(
    value: &ConstantValue<'a>,
    ast: AstBuilder<'a>,
) -> Option<Expression<'a>> {
    match value {
        ConstantValue::Number(n) => {
            // NaN / Infinity / -Infinity have no numeric-literal token; oxc codegen
            // would mishandle a NumericLiteral carrying them. Leave such folds out
            // here (an identifier-based emit would need global-reference safety).
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
        // We do not synthesize BigInt literals or `undefined` here (soundness /
        // identifier-shadowing concerns). The peephole pass may handle these later.
        ConstantValue::BigInt(_) | ConstantValue::Undefined => None,
    }
}

/// The evaluation context required by `oxc_ecmascript`.
///
/// We carry only the `AstBuilder` (the evaluator needs an arena to build interim
/// values). All "treeshake" knobs are set CONSERVATIVELY so folding stays sound:
///   - no global reference is treated as a known constant,
///   - property reads / unknown globals are assumed to have side effects,
///
/// which means `get_side_free_*` (used internally by the evaluator) will refuse
/// to fold anything observable.
struct FoldCtx<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> GlobalContext<'a> for FoldCtx<'a> {
    fn is_global_reference(&self, _reference: &IdentifierReference<'a>) -> bool {
        // We have no whole-program global analysis here; treat nothing as a
        // statically-known global. This is the conservative (sound) choice: it
        // means `undefined`/`NaN`/`Infinity` identifiers are NOT folded away.
        false
    }
}

impl<'a> MayHaveSideEffectsContext<'a> for FoldCtx<'a> {
    fn annotations(&self) -> bool {
        false
    }

    fn manual_pure_functions(&self, _callee: &Expression) -> bool {
        false
    }

    fn property_read_side_effects(&self) -> PropertyReadSideEffects {
        // Assume property reads may have side effects (getters). Conservative.
        PropertyReadSideEffects::All
    }

    fn unknown_global_side_effects(&self) -> bool {
        true
    }
}

impl<'a> ConstantEvaluationCtx<'a> for FoldCtx<'a> {
    fn ast(&self) -> AstBuilder<'a> {
        self.ast
    }
}
