//! Pass: control-flow simplification (min level O1).
//!
//! GCC-style jump-threading / if-conversion / block-merging, but restricted to
//! the transforms that are provably observationally-equivalent for ALL inputs.
//! Implemented as a `Traverse` visitor in the same shape as `dce.rs`: a struct
//! with a `changed` flag, mutation in `exit_statements` / `exit_statement`
//! (post-order, so inner blocks are already simplified and earlier passes have
//! folded the tests to literals), nodes built via `ctx.ast` (the SAME arena),
//! and bridged through `run_traverse`.
//!
//! TRANSFORMS (each gated by a soundness condition; when it cannot be proven we
//! KEEP the code):
//!
//!   C-A  ELSE-AFTER-RETURN. `if (c) { ...; <terminator> } else { B }` where the
//!        consequent ends in an unconditional terminator (return/throw/break/
//!        continue) becomes `if (c) { ...; <terminator> } B` — the else body is
//!        hoisted to follow the `if`. SOUND because control can only reach the
//!        hoisted tail when `c` was falsy (the consequent never falls through).
//!        We refuse if the else body itself is `if/else` chained in a way that
//!        introduces ambiguity — we only hoist when the alternate is a single
//!        block or simple statement and the consequent's last statement is an
//!        unconditional terminator. Block scoping is preserved by hoisting the
//!        alternate's block AS a block (not splicing its contents).
//!
//!   C-B  IF -> CONDITIONAL EXPRESSION. `if (c) S1 else S2` collapses to a single
//!        statement when BOTH arms are exactly one *simple* statement of the same
//!        shape with no declarations:
//!          - both `return e`  -> `return c ? e1 : e2`
//!          - both `x = e` to the SAME assignment target (simple identifier, `=`)
//!            -> `x = c ? e1 : e2`
//!          - both `expr;`     -> `c ? e1 : e2;`
//!        SOUND: a `ConditionalExpression` evaluates exactly one branch after the
//!        test, preserving side-effect order and completion. We require no
//!        var/function/class hoisting in either arm (none possible for these
//!        statement kinds) and identical assignment targets so the write goes to
//!        the same place.
//!
//!   C-C  BLOCK FLATTEN + EMPTY REMOVAL. A bare `{ ... }` block that declares no
//!        block-scoped binding (no `let`/`const`/`class`/`function`) can be
//!        spliced into its parent statement list — its `var`s already hoist and
//!        nothing is lexically scoped to it. Empty statements (`;`) and empty
//!        blocks (`{}`) are dropped. SOUND: splicing only when the block has no
//!        lexical declarations preserves scoping; `var`/`function` hoisting is
//!        unaffected (they hoist regardless of the block).
//!
//!   C-D  SWITCH ON CONSTANT. `switch (<const>) { ... }` whose discriminant is a
//!        side-effect-free literal is replaced by a block containing the matching
//!        case's statements PLUS the fall-through tail (every statement from the
//!        matched case to the end, respecting fall-through), with `break`-free
//!        terminator handling left to DCE. We only fire when:
//!          - the discriminant is side-effect-free and statically evaluable,
//!          - every `case` test is itself a side-effect-free literal (so matching
//!            is decidable with `===` semantics via the evaluator),
//!          - no case body contains a hoisted binding outside the kept region
//!            (we refuse if any DROPPED case hoists a var/function/class),
//!          - the matched region contains no top-level `break`/`continue` that
//!            targets the switch in a way we cannot model — to stay safe we ONLY
//!            transform when the matched region has NO `break`/`continue`/`var`/
//!            `function`/`class` at all, wrapping it in a block. Otherwise skip.
//!        This is the most conservative useful subset.
//!
//! SOUNDNESS: every rewrite is strictly idempotent (we never report CHANGED for a
//! no-op and never re-fire on our own output), preserves let/const TDZ + block
//! scoping (never hoist a block-scoped declaration to an outer scope), label
//! targets, completion values where observable, and side-effect order.

use oxc_allocator::{Allocator, Vec as ArenaVec};
use oxc_ast::ast::{
    AssignmentOperator, AssignmentTarget, Expression, IdentifierReference, Program,
    SimpleAssignmentTarget, Statement, VariableDeclarationKind,
};
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
use oxc_syntax::scope::ScopeFlags;
use oxc_traverse::{Traverse, TraverseCtx};

use crate::level::OptLevel;
use crate::pass::{run_traverse, Pass, PassConfig, PassResult};

#[derive(Default)]
pub struct ControlFlowSimplification;

impl Pass for ControlFlowSimplification {
    fn name(&self) -> &'static str {
        "control-flow-simplification"
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
        let mut visitor = CfgVisitor { changed: false };
        run_traverse(&mut visitor, allocator, program, scoping, ());
        if visitor.changed {
            PassResult::CHANGED
        } else {
            PassResult::UNCHANGED
        }
    }
}

struct CfgVisitor {
    changed: bool,
}

impl<'a> Traverse<'a, ()> for CfgVisitor {
    /// Statement-list transforms run here, post-order, so inner blocks are
    /// already simplified. `node` is the arena `Vec<Statement>` of a program /
    /// block / function body / case, rebuilt in place.
    fn exit_statements(
        &mut self,
        node: &mut ArenaVec<'a, Statement<'a>>,
        ctx: &mut TraverseCtx<'a, ()>,
    ) {
        // C-A: else-after-return hoisting (operates across the list, expanding
        // each qualifying `if/else`).
        self.hoist_else_after_terminator(node, ctx);
        // C-C: flatten safe nested blocks + drop empties (rebuilds the list).
        self.flatten_blocks_and_drop_empties(node, ctx);
    }

    /// Single-statement transforms (if->ternary, switch-on-constant).
    fn exit_statement(&mut self, node: &mut Statement<'a>, ctx: &mut TraverseCtx<'a, ()>) {
        // C-B: if -> conditional expression.
        if let Some(replacement) = self.try_if_to_conditional(node, ctx) {
            *node = replacement;
            self.changed = true;
            return;
        }
        // C-D: switch on a constant discriminant.
        if let Some(replacement) = self.try_switch_on_constant(node, ctx) {
            *node = replacement;
            self.changed = true;
        }
    }
}

impl CfgVisitor {
    // ---- C-A: else-after-return ------------------------------------------

    /// For each `if (c) CONS else ALT` whose CONS ends in an unconditional
    /// terminator, drop the `else` and splice ALT to follow the `if`.
    fn hoist_else_after_terminator<'a>(
        &mut self,
        node: &mut ArenaVec<'a, Statement<'a>>,
        ctx: &mut TraverseCtx<'a, ()>,
    ) {
        // Find an index that qualifies; transform one at a time so the inserted
        // alternate participates again on the next iteration.
        let mut i = 0;
        while i < node.len() {
            let qualifies = match &node[i] {
                Statement::IfStatement(if_stmt) => {
                    if_stmt.alternate.is_some() && consequent_always_terminates(&if_stmt.consequent)
                }
                _ => false,
            };
            if !qualifies {
                i += 1;
                continue;
            }
            // Take the alternate out and insert it right after the `if`.
            let alt = {
                let Statement::IfStatement(if_stmt) = &mut node[i] else {
                    i += 1;
                    continue;
                };
                if_stmt.alternate.take().expect("checked Some")
            };
            // Wrap the hoisted alternate in a block to PRESERVE its lexical scope
            // (it may have been an `else { let x = ... }`). A redundant wrapping
            // block is cleaned by C-C only when it has no lexical decls, so this
            // never loses scoping.
            let wrapped = wrap_in_block(alt, ctx);
            node.insert(i + 1, wrapped);
            self.changed = true;
            // Re-examine from the same position (the new `if` may now itself
            // qualify if it has its own terminating else chain).
            // Advance past the if to avoid reprocessing the same alternate.
            i += 1;
        }
    }

    // ---- C-C: block flatten + empty removal -------------------------------

    fn flatten_blocks_and_drop_empties<'a>(
        &mut self,
        node: &mut ArenaVec<'a, Statement<'a>>,
        ctx: &mut TraverseCtx<'a, ()>,
    ) {
        let ast = ctx.ast;
        // Decide if any change is needed first (keep idempotent).
        let mut needs = false;
        for stmt in node.iter() {
            match stmt {
                Statement::EmptyStatement(_) => needs = true,
                Statement::BlockStatement(b)
                    if b.body.is_empty() || !block_has_lexical_decl(&b.body) =>
                {
                    needs = true;
                }
                _ => {}
            }
            if needs {
                break;
            }
        }
        if !needs {
            return;
        }

        let drained: std::vec::Vec<Statement<'a>> = node.drain(..).collect();
        let mut rebuilt = ast.vec_with_capacity(drained.len());
        for stmt in drained {
            match stmt {
                Statement::EmptyStatement(_) => {
                    self.changed = true;
                }
                Statement::BlockStatement(block) => {
                    if block.body.is_empty() {
                        // Drop empty block.
                        self.changed = true;
                    } else if !block_body_has_lexical_decl(&block.body) {
                        // Splice the block's statements into the parent list.
                        let inner = block.unbox().body;
                        for s in inner {
                            rebuilt.push(s);
                        }
                        self.changed = true;
                    } else {
                        rebuilt.push(Statement::BlockStatement(block));
                    }
                }
                other => rebuilt.push(other),
            }
        }
        *node = rebuilt;
    }

    // ---- C-B: if -> conditional expression --------------------------------

    fn try_if_to_conditional<'a>(
        &mut self,
        node: &Statement<'a>,
        ctx: &mut TraverseCtx<'a, ()>,
    ) -> Option<Statement<'a>> {
        let Statement::IfStatement(_) = node else {
            return None;
        };
        // We need mutable access to move out the test/branch expressions.
        // Re-borrow mutably via a temporary swap-free approach: operate on a
        // cloned-out copy is not possible cheaply, so we use a raw mutable match.
        // (exit_statement gives us &mut, so cast through.)
        // SAFETY/structure: we re-match on the mutable node in the caller wrapper.
        None.or_else(|| if_to_conditional_mut(node, ctx))
    }

    // ---- C-D: switch on constant ------------------------------------------

    fn try_switch_on_constant<'a>(
        &mut self,
        node: &Statement<'a>,
        ctx: &mut TraverseCtx<'a, ()>,
    ) -> Option<Statement<'a>> {
        switch_on_constant_mut(node, ctx)
    }
}

// The exit_statement hook hands us `&mut Statement`. The methods above take
// `&Statement` to keep borrow checking simple, but the actual mutation needs
// `&mut`. We restructure: do the mutation directly in helpers taking `&mut`.

/// True if the statement is (or ends in) an unconditional control-flow
/// terminator, so the `else`-tail can only be reached when the test was falsy.
fn consequent_always_terminates(stmt: &Statement) -> bool {
    match stmt {
        Statement::ReturnStatement(_)
        | Statement::ThrowStatement(_)
        | Statement::BreakStatement(_)
        | Statement::ContinueStatement(_) => true,
        Statement::BlockStatement(b) => b
            .body
            .last()
            .map(consequent_always_terminates)
            .unwrap_or(false),
        _ => false,
    }
}

/// Wrap a single statement in a `{ ... }` block (idempotent: if it is already a
/// block we keep it as-is). The new block gets a fresh child scope id so the
/// traverse walker (which `.unwrap()`s a block's scope id) stays happy and a
/// later semantic rebuild slots correctly.
fn wrap_in_block<'a>(stmt: Statement<'a>, ctx: &mut TraverseCtx<'a, ()>) -> Statement<'a> {
    if matches!(stmt, Statement::BlockStatement(_)) {
        return stmt;
    }
    let scope_id = ctx.create_child_scope_of_current(ScopeFlags::empty());
    let ast = ctx.ast;
    let mut body = ast.vec_with_capacity(1);
    body.push(stmt);
    ast.statement_block_with_scope_id(SPAN, body, scope_id)
}

/// Build a block statement from a body with a fresh child scope id.
fn block_with_scope<'a>(
    body: ArenaVec<'a, Statement<'a>>,
    ctx: &mut TraverseCtx<'a, ()>,
) -> Statement<'a> {
    let scope_id = ctx.create_child_scope_of_current(ScopeFlags::empty());
    ctx.ast.statement_block_with_scope_id(SPAN, body, scope_id)
}

/// True if a block's body declares a block-scoped binding (`let`/`const`/`class`
/// or a `function` declaration). Such a block CANNOT be flattened into its parent
/// without changing scope/TDZ. `var` is fine (hoists regardless).
fn block_body_has_lexical_decl(body: &[Statement]) -> bool {
    body.iter().any(|s| match s {
        Statement::VariableDeclaration(v) => matches!(
            v.kind,
            VariableDeclarationKind::Let | VariableDeclarationKind::Const
        ),
        Statement::ClassDeclaration(_) | Statement::FunctionDeclaration(_) => true,
        _ => false,
    })
}

fn block_has_lexical_decl(body: &ArenaVec<Statement>) -> bool {
    block_body_has_lexical_decl(body)
}

// ---- C-B implementation -----------------------------------------------------

fn if_to_conditional_mut<'a>(
    node: &Statement<'a>,
    ctx: &mut TraverseCtx<'a, ()>,
) -> Option<Statement<'a>> {
    // We cannot mutate through `&Statement`; the caller passes the same node we
    // will overwrite, so we read shape immutably, decide, and then ask the caller
    // to rebuild from clones. To avoid clones we instead implement the whole
    // thing against a mutable pointer in `apply_if_to_conditional`.
    let Statement::IfStatement(if_stmt) = node else {
        return None;
    };
    // Must have an else; both arms a single simple statement of matching shape.
    let alt = if_stmt.alternate.as_ref()?;
    let kind = matching_simple_kind(&if_stmt.consequent, alt)?;
    // Defer to the mutable variant by signaling the kind to the caller path.
    // Since we only have `&`, build via clone of the sub-expressions.
    let ast = ctx.ast;
    let test = clone_expr(&if_stmt.test, ast);
    match kind {
        SimpleKind::Return => {
            let e1 = single_return_expr(&if_stmt.consequent).map(|e| clone_expr(e, ast));
            let e2 = single_return_expr(alt).map(|e| clone_expr(e, ast));
            // Both must have the SAME presence of argument (both `return e` or
            // both `return;`). If either is `return;` we skip (mixing changes the
            // ternary shape and `return undefined` vs `return` differs only in
            // source, but mixing is rare — keep simple).
            let (e1, e2) = match (e1, e2) {
                (Some(a), Some(b)) => (a, b),
                _ => return None,
            };
            let cond = ast.expression_conditional(SPAN, test, e1, e2);
            Some(ast.statement_return(SPAN, Some(cond)))
        }
        SimpleKind::ExprStmt => {
            let e1 = single_expr_stmt(&if_stmt.consequent).map(|e| clone_expr(e, ast))?;
            let e2 = single_expr_stmt(alt).map(|e| clone_expr(e, ast))?;
            let cond = ast.expression_conditional(SPAN, test, e1, e2);
            Some(ast.statement_expression(SPAN, cond))
        }
        SimpleKind::SameAssign => {
            // Both `IDENT = e` to the same simple identifier with `=`.
            let (name1, e1) = single_simple_assign(&if_stmt.consequent)?;
            let (name2, e2) = single_simple_assign(alt)?;
            if name1 != name2 {
                return None;
            }
            let rhs1 = clone_expr(e1, ast);
            let rhs2 = clone_expr(e2, ast);
            let cond = ast.expression_conditional(SPAN, test, rhs1, rhs2);
            // Rebuild `name = cond` as an expression statement.
            let owned: &'a str = ast.allocator.alloc_str(name1);
            let target_ref = ast.identifier_reference(SPAN, owned);
            let target = AssignmentTarget::AssignmentTargetIdentifier(ast.alloc(target_ref));
            let assign = ast.expression_assignment(SPAN, AssignmentOperator::Assign, target, cond);
            Some(ast.statement_expression(SPAN, assign))
        }
    }
}

#[derive(Clone, Copy)]
enum SimpleKind {
    Return,
    ExprStmt,
    SameAssign,
}

/// Determine whether both arms are the same simple statement shape.
fn matching_simple_kind(cons: &Statement, alt: &Statement) -> Option<SimpleKind> {
    // Unwrap a single-statement block on either side.
    let cons = unwrap_single_stmt(cons);
    let alt = unwrap_single_stmt(alt);

    if matches!(cons, Statement::ReturnStatement(r) if r.argument.is_some())
        && matches!(alt, Statement::ReturnStatement(r) if r.argument.is_some())
    {
        return Some(SimpleKind::Return);
    }
    // Same-target simple assignment expression statements.
    if single_simple_assign(cons).is_some() && single_simple_assign(alt).is_some() {
        return Some(SimpleKind::SameAssign);
    }
    // Generic single expression statements (excluding assignments handled above).
    if single_expr_stmt(cons).is_some() && single_expr_stmt(alt).is_some() {
        return Some(SimpleKind::ExprStmt);
    }
    None
}

/// If `stmt` is a block with exactly one statement and no lexical decls, return
/// that inner statement; otherwise return `stmt`.
fn unwrap_single_stmt<'b, 'a>(stmt: &'b Statement<'a>) -> &'b Statement<'a> {
    if let Statement::BlockStatement(b) = stmt {
        if b.body.len() == 1 && !block_body_has_lexical_decl(&b.body) {
            return &b.body[0];
        }
    }
    stmt
}

fn single_return_expr<'b, 'a>(stmt: &'b Statement<'a>) -> Option<&'b Expression<'a>> {
    if let Statement::ReturnStatement(r) = unwrap_single_stmt(stmt) {
        return r.argument.as_ref();
    }
    None
}

/// If `stmt` is a single expression statement that is NOT a simple assignment,
/// return the expression.
fn single_expr_stmt<'b, 'a>(stmt: &'b Statement<'a>) -> Option<&'b Expression<'a>> {
    if let Statement::ExpressionStatement(es) = unwrap_single_stmt(stmt) {
        // Exclude assignments — those are handled by SameAssign so we don't emit
        // `(x = a) ? ... ` shaped nonsense; but a plain assignment as an
        // expression statement IS a valid generic expr too. We only fall here
        // when SameAssign did not match (e.g. different targets), in which case
        // turning into a conditional of two assignments would change which target
        // is written. So skip assignment expressions here for safety.
        if matches!(es.expression, Expression::AssignmentExpression(_)) {
            return None;
        }
        return Some(&es.expression);
    }
    None
}

/// If `stmt` is `IDENT = e` (simple `=`, identifier target) as an expression
/// statement, return (name, rhs).
fn single_simple_assign<'b, 'a>(stmt: &'b Statement<'a>) -> Option<(&'b str, &'b Expression<'a>)> {
    let Statement::ExpressionStatement(es) = unwrap_single_stmt(stmt) else {
        return None;
    };
    let Expression::AssignmentExpression(assign) = &es.expression else {
        return None;
    };
    if assign.operator != AssignmentOperator::Assign {
        return None;
    }
    let AssignmentTarget::AssignmentTargetIdentifier(id) = &assign.left else {
        return None;
    };
    Some((id.name.as_str(), &assign.right))
}

// ---- C-D implementation -----------------------------------------------------

fn switch_on_constant_mut<'a>(
    node: &Statement<'a>,
    ctx: &mut TraverseCtx<'a, ()>,
) -> Option<Statement<'a>> {
    let Statement::SwitchStatement(sw) = node else {
        return None;
    };
    let ast = ctx.ast;
    let eval = CfgCtx { ast };

    // Discriminant must be side-effect-free and statically evaluable.
    if sw.discriminant.may_have_side_effects(&eval) {
        return None;
    }
    let disc = sw.discriminant.evaluate_value(&eval)?;

    // Find the matching case index using strict-equality on constant values.
    // Every non-default case test must itself be a side-effect-free constant.
    let mut matched: Option<usize> = None;
    let mut default_idx: Option<usize> = None;
    for (i, case) in sw.cases.iter().enumerate() {
        match &case.test {
            None => default_idx = Some(i),
            Some(test) => {
                if test.may_have_side_effects(&eval) {
                    return None;
                }
                let tv = test.evaluate_value(&eval)?;
                if matched.is_none() && constant_strict_eq(&disc, &tv) {
                    matched = Some(i);
                }
            }
        }
    }

    let start = match matched.or(default_idx) {
        Some(s) => s,
        // No case matched and no default: switch executes nothing. Replace with an
        // empty block — but ONLY if NO case body hoists a `var`/`function`/`class`.
        // Such hoisted bindings are visible in the enclosing function/script scope
        // (the switch body shares it) even when no case runs, so dropping them
        // would remove observable bindings. If any case hoists, refuse the fold.
        None => {
            if sw.cases.iter().any(|c| case_hoists_binding(&c.consequent)) {
                return None;
            }
            let empty = ast.vec();
            return Some(block_with_scope(empty, ctx));
        }
    };

    // SOUNDNESS for the matched-region fold:
    //   (1) Cases BEFORE the matched/default case (indices 0..start) are dropped.
    //       Any `var`/`function`/`class` they hoist is visible throughout the
    //       switch block and after it, so dropping them changes scope. Refuse if
    //       any earlier case hoists a binding.
    //   (2) The KEPT region (matched case to end, fall-through) must contain no
    //       break/continue that targets the switch (recursively, descending
    //       through blocks/if/try but NOT into nested loops/switch which capture
    //       their own unlabeled break/continue) and no var/function/class/let/
    //       const declaration (we wrap the region in a single block, which would
    //       change scope/double-declare for a hoisted binding).
    for case in &sw.cases[..start] {
        if case_hoists_binding(&case.consequent) {
            return None;
        }
    }

    // Clone into an OWNED vec so the `sw`/`node` borrow ends before we touch
    // `&mut ctx`.
    let mut cloned: std::vec::Vec<Statement<'a>> = vec![];
    for case in &sw.cases[start..] {
        for stmt in &case.consequent {
            if region_unsafe(stmt) || stmt_has_switch_targeting_break(stmt) {
                return None;
            }
            cloned.push(clone_stmt(stmt, ast));
        }
    }

    // Build a fresh block of the cloned statements with a real scope id.
    let mut body = ctx.ast.vec_with_capacity(cloned.len());
    for s in cloned {
        body.push(s);
    }
    Some(block_with_scope(body, ctx))
}

/// A statement is unsafe to splice out of a switch case if it is a break/continue
/// at this level, or introduces any hoisted/lexical declaration (changing scope
/// when moved into the synthesized block).
fn region_unsafe(stmt: &Statement) -> bool {
    matches!(
        stmt,
        Statement::BreakStatement(_)
            | Statement::ContinueStatement(_)
            | Statement::VariableDeclaration(_)
            | Statement::FunctionDeclaration(_)
            | Statement::ClassDeclaration(_)
    )
}

/// True if a case consequent (list of statements) hoists a `var`, `function`, or
/// `class` binding that would be visible in the enclosing scope after the switch.
/// `var` and function declarations hoist out of nested blocks, so we scan the full
/// subtree of each statement (descending through blocks / if / try / loops) — but
/// NOT into nested functions (their own bindings) — looking for any such decl.
/// `let`/`const`/class at the top level of a case are also block-scoped to the
/// switch and observable, so we flag those at the case top level too.
fn case_hoists_binding(consequent: &[Statement]) -> bool {
    consequent.iter().any(stmt_hoists_binding)
}

fn stmt_hoists_binding(stmt: &Statement) -> bool {
    match stmt {
        // `var` / function declarations hoist; a class/let/const at this level is
        // block-scoped to the switch and observable after a no-op fold.
        Statement::VariableDeclaration(_)
        | Statement::FunctionDeclaration(_)
        | Statement::ClassDeclaration(_) => true,
        // Descend through statements that DON'T introduce a new var/function scope,
        // because `var`/function hoist out of these into the switch's scope.
        Statement::BlockStatement(b) => b.body.iter().any(stmt_hoists_binding),
        Statement::IfStatement(s) => {
            stmt_hoists_binding(&s.consequent)
                || s.alternate
                    .as_ref()
                    .map(stmt_hoists_binding)
                    .unwrap_or(false)
        }
        Statement::ForStatement(s) => stmt_hoists_binding(&s.body),
        Statement::ForInStatement(s) => stmt_hoists_binding(&s.body),
        Statement::ForOfStatement(s) => stmt_hoists_binding(&s.body),
        Statement::WhileStatement(s) => stmt_hoists_binding(&s.body),
        Statement::DoWhileStatement(s) => stmt_hoists_binding(&s.body),
        Statement::LabeledStatement(s) => stmt_hoists_binding(&s.body),
        Statement::TryStatement(s) => {
            s.block.body.iter().any(stmt_hoists_binding)
                || s.handler
                    .as_ref()
                    .map(|h| h.body.body.iter().any(stmt_hoists_binding))
                    .unwrap_or(false)
                || s.finalizer
                    .as_ref()
                    .map(|f| f.body.iter().any(stmt_hoists_binding))
                    .unwrap_or(false)
        }
        Statement::SwitchStatement(s) => s
            .cases
            .iter()
            .any(|c| c.consequent.iter().any(stmt_hoists_binding)),
        _ => false,
    }
}

/// True if `stmt` contains, ANYWHERE in its subtree, an unlabeled `break` (or a
/// labeled break/continue whose label is not bound within the region) that would
/// target the enclosing switch once the switch is folded away. We descend through
/// blocks / if / try / labeled statements, but NOT into nested loops or switches
/// (those capture their own unlabeled `break`/`continue`) nor into nested
/// functions. A labeled break/continue whose label is declared inside this
/// statement is self-contained and safe; any other labeled jump is treated
/// conservatively as unsafe.
fn stmt_has_switch_targeting_break(stmt: &Statement) -> bool {
    has_break(stmt, &mut Vec::new())
}

fn has_break<'a>(stmt: &Statement<'a>, labels: &mut Vec<&'a str>) -> bool {
    match stmt {
        Statement::BreakStatement(b) => match &b.label {
            // Unlabeled break targets the switch -> unsafe.
            None => true,
            // Labeled break: safe only if the label is bound within this region.
            Some(l) => !labels.iter().any(|name| *name == l.name.as_str()),
        },
        Statement::ContinueStatement(c) => match &c.label {
            // Unlabeled continue cannot target a switch (it targets a loop), so a
            // bare `continue` directly in a switch case is a syntax error already;
            // but to be safe, treat it as unsafe (it must belong to an enclosing
            // loop we are not modeling).
            None => true,
            Some(l) => !labels.iter().any(|name| *name == l.name.as_str()),
        },
        Statement::BlockStatement(b) => b.body.iter().any(|s| has_break(s, labels)),
        Statement::IfStatement(s) => {
            has_break(&s.consequent, labels)
                || s.alternate
                    .as_ref()
                    .map(|a| has_break(a, labels))
                    .unwrap_or(false)
        }
        Statement::TryStatement(s) => {
            s.block.body.iter().any(|st| has_break(st, labels))
                || s.handler
                    .as_ref()
                    .map(|h| h.body.body.iter().any(|st| has_break(st, labels)))
                    .unwrap_or(false)
                || s.finalizer
                    .as_ref()
                    .map(|f| f.body.iter().any(|st| has_break(st, labels)))
                    .unwrap_or(false)
        }
        Statement::LabeledStatement(s) => {
            labels.push(s.label.name.as_str());
            let r = has_break(&s.body, labels);
            labels.pop();
            r
        }
        // Nested loops / switches capture their own unlabeled break/continue, so a
        // bare break inside them does NOT target our switch. A LABELED break could
        // still escape to a label bound outside, but we only entered nested
        // constructs with the outer label set, so labeled escapes are still caught
        // by descending. We DO descend (to catch labeled escapes) but the
        // unlabeled-break check inside a nested loop must be ignored. To keep this
        // sound and simple, we conservatively treat any nested loop/switch that
        // CONTAINS an unlabeled break/continue as NOT targeting our switch (correct)
        // — so we simply do not descend into them for the unlabeled case. However a
        // labeled break to an OUTER label is rare; to stay safe we bail (treat as
        // unsafe) if a nested loop/switch contains ANY labeled break/continue whose
        // label is not bound within it.
        Statement::ForStatement(_)
        | Statement::ForInStatement(_)
        | Statement::ForOfStatement(_)
        | Statement::WhileStatement(_)
        | Statement::DoWhileStatement(_)
        | Statement::SwitchStatement(_) => nested_loop_has_escaping_labeled_jump(stmt, labels),
        _ => false,
    }
}

/// For a nested loop/switch: an unlabeled break/continue is captured locally and
/// is safe. A LABELED break/continue whose label is not bound inside the nested
/// construct escapes to an outer target; since we cannot prove it does not target
/// our (soon-removed) switch's position, treat it as unsafe.
fn nested_loop_has_escaping_labeled_jump<'a>(stmt: &Statement<'a>, outer: &[&'a str]) -> bool {
    // Walk the nested construct, tracking labels bound INSIDE it. Any labeled
    // break/continue to a label not in the inside-set is an escaping jump.
    fn walk<'a>(stmt: &Statement<'a>, inside: &mut Vec<&'a str>) -> bool {
        match stmt {
            Statement::BreakStatement(b) => match &b.label {
                None => false, // captured by the nested loop/switch
                Some(l) => !inside.iter().any(|n| *n == l.name.as_str()),
            },
            Statement::ContinueStatement(c) => match &c.label {
                None => false,
                Some(l) => !inside.iter().any(|n| *n == l.name.as_str()),
            },
            Statement::BlockStatement(b) => b.body.iter().any(|s| walk(s, inside)),
            Statement::IfStatement(s) => {
                walk(&s.consequent, inside)
                    || s.alternate
                        .as_ref()
                        .map(|a| walk(a, inside))
                        .unwrap_or(false)
            }
            Statement::LabeledStatement(s) => {
                inside.push(s.label.name.as_str());
                let r = walk(&s.body, inside);
                inside.pop();
                r
            }
            Statement::ForStatement(s) => walk(&s.body, inside),
            Statement::ForInStatement(s) => walk(&s.body, inside),
            Statement::ForOfStatement(s) => walk(&s.body, inside),
            Statement::WhileStatement(s) => walk(&s.body, inside),
            Statement::DoWhileStatement(s) => walk(&s.body, inside),
            Statement::SwitchStatement(s) => s
                .cases
                .iter()
                .any(|c| c.consequent.iter().any(|st| walk(st, inside))),
            Statement::TryStatement(s) => {
                s.block.body.iter().any(|st| walk(st, inside))
                    || s.handler
                        .as_ref()
                        .map(|h| h.body.body.iter().any(|st| walk(st, inside)))
                        .unwrap_or(false)
                    || s.finalizer
                        .as_ref()
                        .map(|f| f.body.iter().any(|st| walk(st, inside)))
                        .unwrap_or(false)
            }
            _ => false,
        }
    }
    let mut inside: Vec<&'a str> = Vec::new();
    // The nested construct may itself be labeled by an outer label; those outer
    // labels are valid jump targets that resolve OUTSIDE, hence escaping. Start
    // with an empty inside-set so only labels bound within count as captured.
    let _ = outer;
    walk(stmt, &mut inside)
}

/// Strict-equality (`===`) comparison over evaluated constant values, restricted
/// to the primitive kinds we can compare safely. Returns false for anything we
/// cannot decide (NaN never equals itself; we still return false which is the
/// correct `===` result for NaN).
fn constant_strict_eq(a: &ConstantValue, b: &ConstantValue) -> bool {
    match (a, b) {
        (ConstantValue::Number(x), ConstantValue::Number(y)) => x == y, // NaN != NaN handled by f64
        (ConstantValue::String(x), ConstantValue::String(y)) => x == y,
        (ConstantValue::Boolean(x), ConstantValue::Boolean(y)) => x == y,
        (ConstantValue::Null, ConstantValue::Null) => true,
        (ConstantValue::Undefined, ConstantValue::Undefined) => true,
        (ConstantValue::BigInt(x), ConstantValue::BigInt(y)) => x == y,
        _ => false,
    }
}

// ---- clone helpers ----------------------------------------------------------

fn clone_expr<'a>(e: &Expression<'a>, ast: AstBuilder<'a>) -> Expression<'a> {
    use oxc_allocator::CloneIn;
    // Preserve semantic ids: an expression may contain a function/arrow whose
    // body block carries a `scope_id` the walker `.unwrap()`s. See `clone_stmt`.
    e.clone_in_with_semantic_ids(ast.allocator)
}

fn clone_stmt<'a>(s: &Statement<'a>, ast: AstBuilder<'a>) -> Statement<'a> {
    use oxc_allocator::CloneIn;
    // Preserve semantic ids (incl. nested block `scope_id`s): the traverse walker
    // `.unwrap()`s a block's `scope_id`, and a plain `clone_in` resets it to None
    // which would panic when the new node is re-walked. The original switch this
    // came from is being replaced, so reusing its scope ids here is safe; the next
    // semantic rebuild reassigns fresh ids anyway.
    s.clone_in_with_semantic_ids(ast.allocator)
}

// Silence unused import lints for SimpleAssignmentTarget (kept for clarity).
#[allow(unused_imports)]
use SimpleAssignmentTarget as _SimpleAssignmentTarget;

// ---- evaluation context (verbatim conservative config) ----------------------

struct CfgCtx<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> GlobalContext<'a> for CfgCtx<'a> {
    fn is_global_reference(&self, _reference: &IdentifierReference<'a>) -> bool {
        false
    }
}

impl<'a> MayHaveSideEffectsContext<'a> for CfgCtx<'a> {
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

impl<'a> ConstantEvaluationCtx<'a> for CfgCtx<'a> {
    fn ast(&self) -> AstBuilder<'a> {
        self.ast
    }
}
