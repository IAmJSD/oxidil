//! Pass: dead-store elimination (DSE, min level O2).
//!
//! Removes assignments whose written value is never read. Modeled on GCC's
//! dead-store elimination, but restricted to the subset we can prove
//! observationally equivalent for arbitrary JavaScript.
//!
//! TRANSFORMS (each gated by a soundness condition; when unproven we KEEP the
//! code):
//!
//!   D1  Dead store to a never-read local. A simple assignment `x = e` where the
//!       symbol bound to `x` is a fully-visible local that is WRITTEN but NEVER
//!       READ anywhere in the program (no resolved read reference). Such a store
//!       is dead: the value can never be observed.
//!         - If `e` is provably side-effect-free we replace the assignment
//!           expression with `e` itself (the value of `x = e` is `e`, so this
//!           preserves the expression's value in any context) and, when the
//!           assignment is the whole `ExpressionStatement`, drop the statement
//!           entirely (its value is discarded and `e` is pure).
//!         - If `e` may have side effects we still must evaluate it, so we
//!           replace `x = e` with `e` (dropping only the dead target, never the
//!           computation). In statement position the bare `e` remains as an
//!           `ExpressionStatement`.
//!
//!   D2  Self-assignment `x = x` for a simple local. Evaluating `x = x` reads `x`
//!       then writes the same value back; for a plain identifier target this is a
//!       no-op on observable state (no getter/setter, no coercion). We replace it
//!       with the read `x` (value-preserving) and drop the statement when it is a
//!       standalone `ExpressionStatement`.
//!
//! SOUNDNESS — we only ever touch a SIMPLE IDENTIFIER assignment target
//! (`AssignmentTargetIdentifier`) with the `=` operator. Specifically we SKIP:
//!   - compound assignments (`+=`, `|=`, ...): they READ the target, so the
//!     target is not write-only and the read of the old value is observable.
//!   - member / computed / private stores (`o.x = ...`, `o[k] = ...`): a setter
//!     or proxy trap may run; property writes are never treated as dead.
//!   - destructuring targets.
//!   - ROOT-SCOPE symbols (possibly exported / observed): the existing DCE
//!     convention, since oxc 0.133 has no `Export` flag.
//!   - CLOSED-OVER symbols: a nested closure may read the binding, and our
//!     program-wide read scan already covers that, but we additionally bail on
//!     closed-over symbols so we never reorder/erase a store whose timing a
//!     closure could observe.
//!   - globals / unresolved references (symbol_id == None).
//!   - the whole program when it uses `with` or direct `eval` (scope resolution
//!     and reference counts are unreliable).
//!
//! For D1 the "never read" test is `get_resolved_references(sym)` containing NO
//! read reference (`is_read()` false for every reference). Because oxc only
//! counts a reference as a read when the value is actually consumed, a symbol
//! that is purely write-only is exactly a dead-store candidate. (We rely on the
//! driver rebuilding `Scoping` each O2+ iteration so a binding that becomes
//! never-read only after another pass runs is handled on a later sweep.)
//!
//! IDEMPOTENCE: D1 replaces an `AssignmentExpression` with its RHS (never another
//! assignment to the same dead target), and D2 likewise produces a bare read, so
//! the pass never re-fires on its own output and the driver fixpoint terminates.

use std::collections::HashSet;

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    AssignmentExpression, AssignmentOperator, AssignmentTarget, BindingPattern, Expression,
    IdentifierReference, Program, Statement,
};
use oxc_ast::AstBuilder;
use oxc_ecmascript::constant_evaluation::ConstantEvaluationCtx;
use oxc_ecmascript::side_effects::{
    MayHaveSideEffects, MayHaveSideEffectsContext, PropertyReadSideEffects,
};
use oxc_ecmascript::GlobalContext;
use oxc_semantic::Scoping;
use oxc_span::SPAN;
use oxc_syntax::symbol::{SymbolFlags, SymbolId};
use oxc_traverse::{Traverse, TraverseCtx};

use crate::level::OptLevel;
use crate::pass::{run_traverse, Pass, PassConfig, PassResult};
use crate::semantic_util::{program_has_with_or_eval, symbol_is_closed_over};

#[derive(Default)]
pub struct DeadStoreElimination;

impl Pass for DeadStoreElimination {
    fn name(&self) -> &'static str {
        "dead-store-elimination"
    }

    fn min_level(&self) -> OptLevel {
        OptLevel::O2
    }

    fn run<'a>(
        &mut self,
        program: &mut Program<'a>,
        scoping: &mut Scoping,
        allocator: &'a Allocator,
        _cfg: &PassConfig,
    ) -> PassResult {
        // `with` / direct `eval` defeat scope resolution and make the read/write
        // reference counts unreliable, so DSE is unsound there. Bail entirely.
        if program_has_with_or_eval(program) {
            return PassResult::UNCHANGED;
        }

        // Collect every function/arrow PARAMETER symbol. A parameter that is never
        // read by name can still be observed through a mapped `arguments` object in
        // sloppy mode (`function f(a){ a = 99; return arguments[0]; }`), so removing
        // a write to it is unsound. We conservatively refuse to treat ANY parameter
        // write as dead (the optimization on parameters is niche; correctness wins).
        let params = collect_parameter_symbols(program);

        let mut visitor = DseVisitor {
            changed: false,
            params,
        };
        run_traverse(&mut visitor, allocator, program, scoping, ());
        if visitor.changed {
            PassResult::CHANGED
        } else {
            PassResult::UNCHANGED
        }
    }
}

/// Collect the symbol ids of all function / arrow formal parameters in the
/// program (simple identifiers; destructured/patterned params are covered too by
/// walking their bound identifiers).
fn collect_parameter_symbols(program: &Program) -> HashSet<SymbolId> {
    use oxc_ast_visit::Visit;
    struct ParamCollector {
        out: HashSet<SymbolId>,
    }
    impl<'a> Visit<'a> for ParamCollector {
        // Every formal parameter (function + arrow) is visited here, including
        // each item's binding pattern. We collect all bound identifiers.
        fn visit_formal_parameter(&mut self, it: &oxc_ast::ast::FormalParameter<'a>) {
            collect_pattern_symbols(&it.pattern, &mut self.out);
            oxc_ast_visit::walk::walk_formal_parameter(self, it);
        }
        // A `...rest` parameter is a separate node; collect its bound names too.
        fn visit_binding_rest_element(&mut self, it: &oxc_ast::ast::BindingRestElement<'a>) {
            collect_pattern_symbols(&it.argument, &mut self.out);
            oxc_ast_visit::walk::walk_binding_rest_element(self, it);
        }
    }
    let mut c = ParamCollector {
        out: HashSet::new(),
    };
    c.visit_program(program);
    c.out
}

/// Collect bound symbol ids from a binding pattern (identifier / array / object /
/// assignment patterns), used to gather parameter symbols.
fn collect_pattern_symbols(pat: &BindingPattern, out: &mut HashSet<SymbolId>) {
    match pat {
        BindingPattern::BindingIdentifier(id) => {
            if let Some(sym) = id.symbol_id.get() {
                out.insert(sym);
            }
        }
        BindingPattern::ObjectPattern(o) => {
            for prop in &o.properties {
                collect_pattern_symbols(&prop.value, out);
            }
            if let Some(rest) = &o.rest {
                collect_pattern_symbols(&rest.argument, out);
            }
        }
        BindingPattern::ArrayPattern(a) => {
            for el in a.elements.iter().flatten() {
                collect_pattern_symbols(el, out);
            }
            if let Some(rest) = &a.rest {
                collect_pattern_symbols(&rest.argument, out);
            }
        }
        BindingPattern::AssignmentPattern(ap) => {
            collect_pattern_symbols(&ap.left, out);
        }
    }
}

struct DseVisitor {
    changed: bool,
    params: HashSet<SymbolId>,
}

impl<'a> Traverse<'a, ()> for DseVisitor {
    /// Expression-level handling (runs post-order, BEFORE the enclosing
    /// statement): for a dead store `x = e` (D1) or self-assignment `x = x` (D2)
    /// that appears as a SUB-expression, replace it with its RHS, preserving the
    /// assignment's value (`x = e` evaluates to `e`) while erasing the dead write.
    ///
    /// We SKIP the case where the assignment is the direct expression of an
    /// `ExpressionStatement`: there the value is discarded, so `exit_statement`
    /// handles it (and can drop the whole statement when the RHS is pure). This
    /// split avoids the two hooks fighting over the same node.
    fn exit_expression(&mut self, node: &mut Expression<'a>, ctx: &mut TraverseCtx<'a, ()>) {
        if ctx.parent().is_expression_statement() {
            return;
        }
        let Expression::AssignmentExpression(assign) = node else {
            return;
        };
        match classify(assign, ctx.scoping(), &self.params) {
            // Replace `x = e` / `x = x` with `e` / `x` (the RHS), preserving value.
            Some(DeadKind::DeadStore) | Some(DeadKind::SelfAssign) => {
                let rhs =
                    std::mem::replace(&mut assign.right, ctx.ast.expression_null_literal(SPAN));
                *node = rhs;
                self.changed = true;
            }
            None => {}
        }
    }

    /// Statement-level handling for a standalone `ExpressionStatement` whose
    /// expression is a dead store / self-assignment. The value is discarded here:
    ///   - self-assignment, or dead store with a PURE RHS  => drop the statement.
    ///   - dead store with an EFFECTFUL RHS                => keep the RHS as the
    ///     statement's expression (the computation must run), dropping only the
    ///     dead target.
    fn exit_statement(&mut self, node: &mut Statement<'a>, ctx: &mut TraverseCtx<'a, ()>) {
        let Statement::ExpressionStatement(expr_stmt) = node else {
            return;
        };
        let Expression::AssignmentExpression(assign) = &mut expr_stmt.expression else {
            return;
        };
        let kind = match classify(assign, ctx.scoping(), &self.params) {
            Some(k) => k,
            None => return,
        };
        let eval = DseCtx { ast: ctx.ast };
        match kind {
            DeadKind::SelfAssign => {
                *node = ctx.ast.statement_empty(SPAN);
                self.changed = true;
            }
            DeadKind::DeadStore if !assign.right.may_have_side_effects(&eval) => {
                *node = ctx.ast.statement_empty(SPAN);
                self.changed = true;
            }
            DeadKind::DeadStore => {
                // Effectful RHS: keep `e`, drop the dead target.
                let rhs =
                    std::mem::replace(&mut assign.right, ctx.ast.expression_null_literal(SPAN));
                expr_stmt.expression = rhs;
                self.changed = true;
            }
        }
    }
}

enum DeadKind {
    /// `x = e` where `x` is a never-read local.
    DeadStore,
    /// `x = x` for a simple local (no observable effect).
    SelfAssign,
}

/// Classify an assignment as a removable dead store / self-assignment, or `None`
/// when any soundness precondition is unproven.
fn classify(
    assign: &AssignmentExpression,
    scoping: &Scoping,
    params: &HashSet<SymbolId>,
) -> Option<DeadKind> {
    // Only the plain `=` operator. Compound ops read the target.
    if assign.operator != AssignmentOperator::Assign {
        return None;
    }
    // Only a simple identifier target: never a member/computed/private store
    // (getters/setters/proxies) or a destructuring pattern.
    let AssignmentTarget::AssignmentTargetIdentifier(target) = &assign.left else {
        return None;
    };
    let symbol_id = resolve_write_symbol(target, scoping)?;

    // Never touch root-scope (possibly exported / observed) bindings.
    if scoping.symbol_scope_id(symbol_id) == scoping.root_scope_id() {
        return None;
    }
    // A closure could observe the store's value/timing.
    if symbol_is_closed_over(symbol_id, scoping) {
        return None;
    }

    // LEXICAL (let/const/class) TARGET GUARD. An assignment to such a binding is
    // NOT a removable dead store, because the WRITE ITSELF can throw and that
    // throw is observable:
    //   * `const x` target  -> assignment ALWAYS throws TypeError (even `x = x`);
    //   * `let`/`const`/class target written BEFORE its declaration is reached
    //     -> throws ReferenceError (Temporal Dead Zone).
    // We cannot cheaply prove the write site is after the declaration, so we
    // conservatively refuse to treat ANY write to a block-scoped binding as dead
    // or as a no-op self-assign. Function-scoped `var` (no const, no TDZ) is
    // unaffected and remains a valid DSE target.
    let flags = scoping.symbol_flags(symbol_id);
    if flags.intersects(
        SymbolFlags::ConstVariable | SymbolFlags::BlockScopedVariable | SymbolFlags::Class,
    ) {
        return None;
    }

    // PARAMETER / mapped-`arguments` GUARD. In a sloppy-mode function with a simple
    // parameter list the formal parameter and `arguments[i]` are aliased, so a
    // write to the parameter by name is observable through `arguments` even when
    // the name is never read. We cannot cheaply prove the enclosing function is
    // strict, so we refuse to treat any parameter write as dead.
    if params.contains(&symbol_id) {
        return None;
    }

    // D2: self-assignment `x = x` to the SAME symbol.
    if let Expression::Identifier(rhs_id) = &assign.right {
        if let Some(rhs_symbol) = resolve_read_symbol(rhs_id, scoping) {
            if rhs_symbol == symbol_id {
                return Some(DeadKind::SelfAssign);
            }
        }
    }

    // D1: the target symbol is never read anywhere -> the store is dead.
    let any_read = scoping
        .get_resolved_references(symbol_id)
        .any(|r| r.is_read());
    if !any_read {
        return Some(DeadKind::DeadStore);
    }

    None
}

/// Resolve the symbol of a write-target identifier.
fn resolve_write_symbol(id: &IdentifierReference, scoping: &Scoping) -> Option<SymbolId> {
    let rid = id.reference_id.get()?;
    scoping.get_reference(rid).symbol_id()
}

/// Resolve the symbol of a read identifier reference.
fn resolve_read_symbol(id: &IdentifierReference, scoping: &Scoping) -> Option<SymbolId> {
    let rid = id.reference_id.get()?;
    scoping.get_reference(rid).symbol_id()
}

/// Evaluation / side-effect context. Same conservative knobs as the folding,
/// peephole, and DCE passes: no known globals, property reads and unknown globals
/// assumed to have side effects — so `may_have_side_effects` is pessimistic
/// (safe).
struct DseCtx<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> GlobalContext<'a> for DseCtx<'a> {
    fn is_global_reference(&self, _reference: &IdentifierReference<'a>) -> bool {
        false
    }
}

impl<'a> MayHaveSideEffectsContext<'a> for DseCtx<'a> {
    fn annotations(&self) -> bool {
        false
    }

    fn manual_pure_functions(&self, _callee: &Expression) -> bool {
        false
    }

    fn property_read_side_effects(&self) -> PropertyReadSideEffects {
        PropertyReadSideEffects::All
    }

    fn unknown_global_side_effects(&self) -> bool {
        true
    }
}

impl<'a> ConstantEvaluationCtx<'a> for DseCtx<'a> {
    fn ast(&self) -> AstBuilder<'a> {
        self.ast
    }
}
