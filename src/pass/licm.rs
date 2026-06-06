//! Pass: loop-invariant code motion (LICM), PURE-ONLY and conservative (min level O3).
//!
//! In the spirit of GCC's LICM, but restricted to the subset we can prove
//! observationally equivalent for arbitrary JavaScript. We hoist a computation
//! out of a loop ONLY when:
//!   * the WHOLE expression subtree is side-effect-free (`may_have_side_effects
//!     == false`), and
//!   * every operand is loop-invariant: a primitive literal, or an identifier
//!     read of a resolved LOCAL symbol that is NEVER mutated anywhere in the
//!     program (`symbol_is_mutated == false`), and
//!   * the expression is non-trivial (>= 2 operator nodes), so hoisting actually
//!     pays for the extra temp.
//!
//! ELIGIBLE EXPRESSIONS
//! --------------------
//! Only Binary / Logical / Unary operator trees whose entire subtree is built
//! from literals + never-mutated local identifier reads. We EXCLUDE all calls,
//! `new`, member / computed / private access (a getter could run code — the
//! shared Ctx sets `property_read_side_effects = All`), `this`, templates,
//! sequences, assignments, `await`/`yield`. This is exactly the eligibility used
//! by `cse-gvn`, re-applied here.
//!
//! SOUNDNESS — why hoisting BEFORE a possibly-zero-iteration loop is safe
//! ---------------------------------------------------------------------
//!  * NON-THROWING: because the whole subtree must report
//!    `may_have_side_effects == false`, oxc has proven it cannot trigger a
//!    coercion (`valueOf`/`toString`), a getter, or any user code — and over
//!    operands of unknown type oxc reports arithmetic/bitwise ops as POSSIBLY
//!    impure (operand coercion), so those are rejected. What survives genuinely
//!    cannot throw. Hoisting it ahead of a loop that may execute zero times
//!    therefore introduces no observable behavior (no new throw, no new effect).
//!  * INVARIANT: every operand is a literal or a never-(re)assigned local, so the
//!    value is identical on every iteration and at the pre-loop point. No
//!    intervening assignment, no aliasing, and (member access excluded) no getter
//!    can change it between the pre-loop site and any use inside the body.
//!  * The hoisted binding is a fresh UID (`generate_uid_in_current_scope`) so it
//!    cannot collide with or shadow any user binding, and `const` so it is itself
//!    never reassigned.
//!  * break / continue / labels do NOT affect correctness: we move only a pure,
//!    non-throwing value computation; whether or not the loop body reaches the
//!    original use, the pre-computed value is identical and side-effect-free.
//!  * We bail entirely on `with` / direct `eval` (scope resolution + mutation
//!    queries unreliable).
//!
//! IDEMPOTENCE / TERMINATION
//! -------------------------
//! After hoisting, every occurrence in the body is a plain identifier read of the
//! temp — no longer an eligible candidate (a single identifier is < 2 operator
//! nodes). So the pass cannot re-fire on its own output and the fixpoint
//! terminates. min_level O3 means it never runs at O1/O2/Os.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use oxc_allocator::{Allocator, CloneIn, Vec as ArenaVec};
use oxc_ast::ast::{Expression, IdentifierReference, Program, Statement, VariableDeclarationKind};
use oxc_ast::AstBuilder;
use oxc_ecmascript::constant_evaluation::ConstantEvaluationCtx;
use oxc_ecmascript::side_effects::{MayHaveSideEffectsContext, PropertyReadSideEffects};
use oxc_ecmascript::GlobalContext;
use oxc_semantic::Scoping;
use oxc_span::{GetSpan, SPAN};
use oxc_syntax::symbol::{SymbolFlags, SymbolId};
use oxc_traverse::{Traverse, TraverseCtx};

use crate::level::OptLevel;
use crate::pass::{run_traverse, Pass, PassConfig, PassResult};
use crate::semantic_util::{
    collect_provably_number_symbols, expr_is_provably_number, program_has_with_or_eval,
};

#[derive(Default)]
pub struct Licm;

impl Pass for Licm {
    fn name(&self) -> &'static str {
        "licm"
    }

    fn min_level(&self) -> OptLevel {
        OptLevel::O3
    }

    fn run<'a>(
        &mut self,
        program: &mut Program<'a>,
        scoping: &mut Scoping,
        allocator: &'a Allocator,
        _cfg: &PassConfig,
    ) -> PassResult {
        // `with` / direct `eval` make scope resolution and `symbol_is_mutated`
        // unreliable, so we cannot prove operand invariance. Bail.
        if program_has_with_or_eval(program) {
            return PassResult::UNCHANGED;
        }

        // Whole-program set of bindings that PROVABLY hold a finite Number at every
        // program point (so reading them runs no coercion hook and the coercing
        // operators are pure value computations over them). This lets `build_key`
        // broaden the hoistable operator set beyond strict-equality WITHOUT risking
        // any change to coercion count/order or any new throw. Empty under
        // with/eval (already bailed above).
        let prim = Rc::new(collect_provably_number_symbols(program, scoping));

        let mut visitor = LicmVisitor {
            changed: false,
            prim,
        };
        run_traverse(&mut visitor, allocator, program, scoping, ());
        if visitor.changed {
            PassResult::CHANGED
        } else {
            PassResult::UNCHANGED
        }
    }
}

struct LicmVisitor {
    changed: bool,
    /// Symbols provably holding a finite Number (see `run`). Operands drawn only
    /// from this set + numeric literals make the coercing operators safe to hoist.
    prim: Rc<HashSet<SymbolId>>,
}

impl<'a> Traverse<'a, ()> for LicmVisitor {
    /// We operate at the statement-list level so we can insert the hoisted `const`
    /// declaration immediately before the loop statement, in the SAME list.
    fn exit_statements(
        &mut self,
        node: &mut ArenaVec<'a, Statement<'a>>,
        ctx: &mut TraverseCtx<'a, ()>,
    ) {
        // Collect (statement-index, hoisted-declaration) pairs to insert. We
        // process loops left-to-right but defer insertion so indices stay valid.
        let mut inserts: Vec<(usize, Statement<'a>)> = Vec::new();

        let n = node.len();
        for i in 0..n {
            // Only loop statements are candidates.
            if !is_loop_statement(&node[i]) {
                continue;
            }

            // Move the loop statement out so we can borrow its body mutably while
            // also using `ctx` mutably (no overlap with `node`).
            let mut loop_stmt = std::mem::replace(&mut node[i], ctx.ast.statement_empty(SPAN));

            // Find eligible invariant expressions in the loop body, grouped by
            // structural key. Each group is hoisted once.
            let mut groups: HashMap<String, Group<'a>> = HashMap::new();
            let loop_start = loop_stmt.span().start;
            if let Some(body) = loop_body_mut(&mut loop_stmt) {
                let eval = LicmCtx { ast: ctx.ast };
                let mut collector = Collector {
                    scoping: ctx.scoping(),
                    eval: &eval,
                    groups: &mut groups,
                    loop_start,
                    prim: &self.prim,
                };
                collector.scan_statement(body);
            }

            if groups.is_empty() {
                node[i] = loop_stmt;
                continue;
            }

            // Deterministic order: sort eligible groups by key.
            let mut eligible: Vec<(String, Group<'a>)> = groups.into_iter().collect();
            eligible.sort_by(|a, b| a.0.cmp(&b.0));

            // Mint a temp per group and build the plan.
            struct Plan<'a> {
                key: String,
                binding: oxc_traverse::BoundIdentifier<'a>,
                template: Expression<'a>,
            }
            let mut plans: Vec<Plan<'a>> = Vec::with_capacity(eligible.len());
            for (key, g) in eligible {
                let binding =
                    ctx.generate_uid_in_current_scope("licm", SymbolFlags::BlockScopedVariable);
                plans.push(Plan {
                    key,
                    binding,
                    template: g.template,
                });
            }

            // Substitute every occurrence in the body with a read of its temp.
            let keys: HashMap<String, usize> = plans
                .iter()
                .enumerate()
                .map(|(idx, p)| (p.key.clone(), idx))
                .collect();
            let bindings: Vec<oxc_traverse::BoundIdentifier<'a>> =
                plans.iter().map(|p| p.binding.clone()).collect();
            let scoping_ptr: *const Scoping = ctx.scoping();
            if let Some(body) = loop_body_mut(&mut loop_stmt) {
                substitute_in_statement(body, &keys, &bindings, scoping_ptr, &self.prim, ctx);
            }

            // Build `const <temp> = <template>;` for each plan; these go BEFORE the
            // loop (lower indices first so dependency order is preserved). We emit
            // them in plan order (already sorted by key, deterministic).
            for p in plans {
                let id = p.binding.create_binding_pattern(ctx);
                let declarator = ctx.ast.variable_declarator(
                    SPAN,
                    VariableDeclarationKind::Const,
                    id,
                    oxc_ast::NONE,
                    Some(p.template),
                    false,
                );
                let mut decls = ctx.ast.vec_with_capacity(1);
                decls.push(declarator);
                let var_decl = ctx.ast.alloc_variable_declaration(
                    SPAN,
                    VariableDeclarationKind::Const,
                    decls,
                    false,
                );
                inserts.push((i, Statement::VariableDeclaration(var_decl)));
            }

            node[i] = loop_stmt;
            self.changed = true;
        }

        if inserts.is_empty() {
            return;
        }

        // Insert each hoisted declaration before its target loop. Process from the
        // highest index downward so earlier insertions do not shift later targets.
        // For equal index, the natural Vec order keeps temps in plan (key) order.
        inserts.sort_by_key(|a| std::cmp::Reverse(a.0));
        for (idx, stmt) in inserts {
            node.insert(idx, stmt);
        }
    }
}

/// A group of structurally-identical eligible occurrences inside one loop body.
struct Group<'a> {
    template: Expression<'a>,
}

fn is_loop_statement(stmt: &Statement) -> bool {
    matches!(
        stmt,
        Statement::ForStatement(_)
            | Statement::WhileStatement(_)
            | Statement::DoWhileStatement(_)
            | Statement::ForInStatement(_)
            | Statement::ForOfStatement(_)
    )
}

/// Mutable access to a loop statement's body. The body is the only region whose
/// expressions we scan/hoist; the loop header (test/update/init/right) is NOT
/// touched (it may be evaluated conditionally or define the induction var).
fn loop_body_mut<'a, 'b>(stmt: &'b mut Statement<'a>) -> Option<&'b mut Statement<'a>> {
    match stmt {
        Statement::ForStatement(f) => Some(&mut f.body),
        Statement::WhileStatement(w) => Some(&mut w.body),
        Statement::DoWhileStatement(d) => Some(&mut d.body),
        Statement::ForInStatement(f) => Some(&mut f.body),
        Statement::ForOfStatement(f) => Some(&mut f.body),
        _ => None,
    }
}

/// Walks a loop body collecting eligible invariant candidates. Because operands
/// must be never-mutated-anywhere locals (or literals), an occurrence is invariant
/// regardless of its position, so we may descend through nested statements/blocks.
/// We do NOT descend into nested functions (a closure has its own scope/timing) or
/// into nested loops (their own `exit_statements` handles them).
struct Collector<'a, 'c, 's> {
    scoping: &'s Scoping,
    eval: &'c LicmCtx<'a>,
    groups: &'c mut HashMap<String, Group<'a>>,
    /// Source offset of the loop statement; operands whose lexical declaration is
    /// at/after this offset are TDZ-unsafe to hoist (see `build_key_pos`).
    loop_start: u32,
    prim: &'c HashSet<SymbolId>,
}

impl<'a, 'c, 's> Collector<'a, 'c, 's> {
    fn scan_statement(&mut self, stmt: &Statement<'a>) {
        match stmt {
            Statement::ExpressionStatement(es) => self.scan_expr(&es.expression),
            Statement::VariableDeclaration(vd) => {
                for d in &vd.declarations {
                    if let Some(init) = &d.init {
                        self.scan_expr(init);
                    }
                }
            }
            Statement::ReturnStatement(rs) => {
                if let Some(arg) = &rs.argument {
                    self.scan_expr(arg);
                }
            }
            Statement::IfStatement(s) => {
                self.scan_expr(&s.test);
                self.scan_statement(&s.consequent);
                if let Some(alt) = &s.alternate {
                    self.scan_statement(alt);
                }
            }
            Statement::BlockStatement(b) => {
                for s in &b.body {
                    self.scan_statement(s);
                }
            }
            // Any other statement kind (nested loops, switch, try, functions, etc.)
            // is left to its own handling / treated opaquely. Conservative.
            _ => {}
        }
    }

    fn scan_expr(&mut self, expr: &Expression<'a>) {
        if let Some(key) =
            eligible_key_at(expr, self.scoping, self.eval, self.prim, self.loop_start)
        {
            self.record(&key, expr);
        }
        // Recurse only into always-evaluated children (matching cse-gvn): not into
        // logical RHS / conditional branches / call args / function bodies.
        match expr {
            Expression::BinaryExpression(b) => {
                self.scan_expr(&b.left);
                self.scan_expr(&b.right);
            }
            Expression::UnaryExpression(u) => self.scan_expr(&u.argument),
            Expression::LogicalExpression(l) => self.scan_expr(&l.left),
            Expression::ParenthesizedExpression(p) => self.scan_expr(&p.expression),
            _ => {}
        }
    }

    fn record(&mut self, key: &str, expr: &Expression<'a>) {
        self.groups.entry(key.to_string()).or_insert_with(|| Group {
            template: expr.clone_in(self.eval.ast.allocator),
        });
    }
}

/// Substitute occurrences inside a statement (mirrors the collector's walk).
fn substitute_in_statement<'a>(
    stmt: &mut Statement<'a>,
    keys: &HashMap<String, usize>,
    bindings: &[oxc_traverse::BoundIdentifier<'a>],
    scoping_ptr: *const Scoping,
    prim: &HashSet<SymbolId>,
    ctx: &mut TraverseCtx<'a, ()>,
) {
    match stmt {
        Statement::ExpressionStatement(es) => {
            substitute_in_expr(&mut es.expression, keys, bindings, scoping_ptr, prim, ctx)
        }
        Statement::VariableDeclaration(vd) => {
            for d in vd.declarations.iter_mut() {
                if let Some(init) = &mut d.init {
                    substitute_in_expr(init, keys, bindings, scoping_ptr, prim, ctx);
                }
            }
        }
        Statement::ReturnStatement(rs) => {
            if let Some(arg) = &mut rs.argument {
                substitute_in_expr(arg, keys, bindings, scoping_ptr, prim, ctx);
            }
        }
        Statement::IfStatement(s) => {
            substitute_in_expr(&mut s.test, keys, bindings, scoping_ptr, prim, ctx);
            substitute_in_statement(&mut s.consequent, keys, bindings, scoping_ptr, prim, ctx);
            if let Some(alt) = &mut s.alternate {
                substitute_in_statement(alt, keys, bindings, scoping_ptr, prim, ctx);
            }
        }
        Statement::BlockStatement(b) => {
            for s in b.body.iter_mut() {
                substitute_in_statement(s, keys, bindings, scoping_ptr, prim, ctx);
            }
        }
        _ => {}
    }
}

fn substitute_in_expr<'a>(
    expr: &mut Expression<'a>,
    keys: &HashMap<String, usize>,
    bindings: &[oxc_traverse::BoundIdentifier<'a>],
    scoping_ptr: *const Scoping,
    prim: &HashSet<SymbolId>,
    ctx: &mut TraverseCtx<'a, ()>,
) {
    // SAFETY: `scoping_ptr` aliases `ctx.scoping()`; we only use it for the
    // immutable structural-key read and never hold it across a mutation of scoping.
    let scoping: &Scoping = unsafe { &*scoping_ptr };
    let eval = LicmCtx { ast: ctx.ast };
    if let Some(key) = eligible_key(expr, scoping, &eval, prim) {
        if let Some(&idx) = keys.get(&key) {
            *expr = bindings[idx].create_read_expression(ctx);
            return;
        }
    }
    match expr {
        Expression::BinaryExpression(b) => {
            substitute_in_expr(&mut b.left, keys, bindings, scoping_ptr, prim, ctx);
            substitute_in_expr(&mut b.right, keys, bindings, scoping_ptr, prim, ctx);
        }
        Expression::UnaryExpression(u) => {
            substitute_in_expr(&mut u.argument, keys, bindings, scoping_ptr, prim, ctx)
        }
        Expression::LogicalExpression(l) => {
            substitute_in_expr(&mut l.left, keys, bindings, scoping_ptr, prim, ctx)
        }
        Expression::ParenthesizedExpression(p) => {
            substitute_in_expr(&mut p.expression, keys, bindings, scoping_ptr, prim, ctx)
        }
        _ => {}
    }
}

/// Compute a structural key for `expr` if it is an eligible LICM candidate, else
/// `None`. Eligible == pure, non-trivial (>= 2 operator nodes), built only from
/// literals + never-mutated local identifier reads. Identical eligibility to
/// `cse-gvn`.
fn eligible_key(
    expr: &Expression,
    scoping: &Scoping,
    eval: &LicmCtx,
    prim: &HashSet<SymbolId>,
) -> Option<String> {
    eligible_key_at(expr, scoping, eval, prim, 0)
}

/// As `eligible_key`, but `loop_start` enables the TDZ-hoist guard on operands
/// (reject block-scoped operands declared at/after the loop). Pass 0 to disable
/// (substitution path: keys already proven eligible at collection time).
fn eligible_key_at(
    expr: &Expression,
    scoping: &Scoping,
    eval: &LicmCtx,
    prim: &HashSet<SymbolId>,
    loop_start: u32,
) -> Option<String> {
    if !matches!(
        expr,
        Expression::BinaryExpression(_)
            | Expression::LogicalExpression(_)
            | Expression::UnaryExpression(_)
    ) {
        return None;
    }
    // SOUNDNESS GATE: `build_key_pos` is now the COMPLETE, load-bearing proof
    // (identical to cse-gvn). It admits a node ONLY if it is a primitive literal, a
    // never-mutated LOCAL identifier read, or one of the restricted operator set,
    // AND — for any operator that would coerce (`+ - * / % ** & | ^ << >> >>> <
    // > <= >=`, unary `+ - ~`) — ONLY when every operand is PROVABLY a finite
    // Number (`prim`), so no ToPrimitive/ToNumber hook can run and number-op-number
    // cannot throw on type. Every node it accepts is therefore provably
    // side-effect-free, with an INVARIANT value across the loop, so hoisting it
    // ahead of a possibly-zero-iteration loop introduces no observable coercion and
    // no new throw.
    //
    // We deliberately do NOT use `oxc::may_have_side_effects` as the gate: over
    // identifier operands of unknown type it conservatively reports the coercing
    // operators as POSSIBLY-impure (it cannot see our whole-program provably-Number
    // proof), which would reject the very expressions this relaxation is meant to
    // hoist. `build_key_pos` is strictly stronger for our node set, so relying on
    // it alone is sound and is what enables the broadening.
    let _ = eval;
    let mut ops = 0usize;
    let mut key = String::new();
    if !build_key_pos(expr, scoping, prim, &mut key, &mut ops, loop_start) {
        return None;
    }
    if ops < 2 {
        return None;
    }
    Some(key)
}

/// `loop_start` is the source offset of the loop statement being hoisted out of.
/// An identifier operand whose lexical (let/const/class) declaration appears AFTER
/// this offset would be in its Temporal Dead Zone at the pre-loop hoist site, so
/// reading it there could throw — we reject such operands. `loop_start == 0`
/// disables the check (used by the substitution walk, which only matches keys
/// already proven eligible).
fn build_key_pos(
    expr: &Expression,
    scoping: &Scoping,
    prim: &HashSet<SymbolId>,
    key: &mut String,
    ops: &mut usize,
    loop_start: u32,
) -> bool {
    use oxc_syntax::operator::{BinaryOperator, UnaryOperator};
    use std::fmt::Write;
    match expr {
        Expression::BinaryExpression(b) => {
            // COERCION + THROW GUARD (mirrors cse-gvn). Strict equality never
            // coerces, so it is always safe. The arithmetic / bitwise / relational
            // operators each invoke ToPrimitive/ToNumber on a non-primitive operand
            // (running user code) AND can throw on a BigInt/Number mix — so they are
            // safe to hoist ahead of a possibly-zero-iteration loop ONLY when BOTH
            // operands are provably finite Numbers: then no hook runs, `+` is purely
            // numeric, and number-op-number never throws (division by zero yields
            // Infinity/NaN, not a throw). The value is invariant, so pre-computing it
            // once introduces no observable coercion and no new throw. Loose equality
            // (`==`/`!=`), `in`, and `instanceof` are NOT broadened.
            let is_strict_eq = matches!(
                b.operator,
                BinaryOperator::StrictEquality | BinaryOperator::StrictInequality
            );
            let is_numeric_op = matches!(
                b.operator,
                BinaryOperator::Addition
                    | BinaryOperator::Subtraction
                    | BinaryOperator::Multiplication
                    | BinaryOperator::Division
                    | BinaryOperator::Remainder
                    | BinaryOperator::Exponential
                    | BinaryOperator::BitwiseAnd
                    | BinaryOperator::BitwiseOR
                    | BinaryOperator::BitwiseXOR
                    | BinaryOperator::ShiftLeft
                    | BinaryOperator::ShiftRight
                    | BinaryOperator::ShiftRightZeroFill
                    | BinaryOperator::LessThan
                    | BinaryOperator::GreaterThan
                    | BinaryOperator::LessEqualThan
                    | BinaryOperator::GreaterEqualThan
            );
            if is_numeric_op {
                if !(expr_is_provably_number(&b.left, scoping, prim)
                    && expr_is_provably_number(&b.right, scoping, prim))
                {
                    return false;
                }
            } else if !is_strict_eq {
                return false;
            }
            *ops += 1;
            let _ = write!(key, "(B{:?} ", b.operator);
            if !build_key_pos(&b.left, scoping, prim, key, ops, loop_start) {
                return false;
            }
            key.push(' ');
            if !build_key_pos(&b.right, scoping, prim, key, ops, loop_start) {
                return false;
            }
            key.push(')');
            true
        }
        Expression::LogicalExpression(l) => {
            *ops += 1;
            let _ = write!(key, "(L{:?} ", l.operator);
            if !build_key_pos(&l.left, scoping, prim, key, ops, loop_start) {
                return false;
            }
            key.push(' ');
            if !build_key_pos(&l.right, scoping, prim, key, ops, loop_start) {
                return false;
            }
            key.push(')');
            true
        }
        Expression::UnaryExpression(u) => {
            // `!`/typeof/void never coerce via valueOf/toString — always safe.
            // `+x`/`-x`/`~x` invoke ToNumber UNLESS the argument is provably a finite
            // Number, in which case no hook runs and the op cannot throw. `delete`
            // has side effects, so always reject it.
            let coercion_free = matches!(
                u.operator,
                UnaryOperator::LogicalNot | UnaryOperator::Typeof | UnaryOperator::Void
            );
            let numeric_unary = matches!(
                u.operator,
                UnaryOperator::UnaryNegation | UnaryOperator::UnaryPlus | UnaryOperator::BitwiseNot
            );
            if numeric_unary {
                if !expr_is_provably_number(&u.argument, scoping, prim) {
                    return false;
                }
            } else if !coercion_free {
                return false;
            }
            *ops += 1;
            let _ = write!(key, "(U{:?} ", u.operator);
            if !build_key_pos(&u.argument, scoping, prim, key, ops, loop_start) {
                return false;
            }
            key.push(')');
            true
        }
        Expression::ParenthesizedExpression(p) => {
            build_key_pos(&p.expression, scoping, prim, key, ops, loop_start)
        }
        Expression::Identifier(id) => {
            // Must resolve to a LOCAL symbol that is never mutated anywhere.
            let Some(rid) = id.reference_id.get() else {
                return false;
            };
            let Some(sym) = scoping.get_reference(rid).symbol_id() else {
                return false; // unresolved / global => could change; disqualify.
            };
            if scoping.symbol_is_mutated(sym) {
                return false;
            }
            // TDZ HOIST GUARD. The hoisted `const _licm = <expr>` is inserted
            // immediately BEFORE the loop and runs unconditionally. If this operand
            // is a block-scoped (let/const/class) binding declared AFTER the loop,
            // it is in its Temporal Dead Zone at the hoist site: reading it there
            // throws ReferenceError, a throw the original (which only reads it
            // inside the loop body) need not produce when the loop runs zero times.
            // Accept the operand only if it is NOT block-scoped, OR its declaration
            // begins strictly before the loop. `var`/function bindings are hoisted-
            // initialized, so they are always safe.
            if loop_start != 0 {
                let flags = scoping.symbol_flags(sym);
                let block_scoped = flags.intersects(
                    oxc_syntax::symbol::SymbolFlags::BlockScopedVariable
                        | oxc_syntax::symbol::SymbolFlags::ConstVariable
                        | oxc_syntax::symbol::SymbolFlags::Class,
                );
                if block_scoped {
                    let decl_start = scoping.symbol_span(sym).start;
                    if decl_start >= loop_start {
                        return false;
                    }
                }
            }
            let _ = write!(key, "#{}", sym.index());
            true
        }
        Expression::NumericLiteral(n) => {
            let _ = write!(key, "n{:016x}", n.value.to_bits());
            true
        }
        Expression::StringLiteral(s) => {
            let _ = write!(key, "s{:?}", s.value.as_str());
            true
        }
        Expression::BooleanLiteral(b) => {
            let _ = write!(key, "b{}", b.value);
            true
        }
        Expression::NullLiteral(_) => {
            key.push_str("null");
            true
        }
        Expression::BigIntLiteral(bi) => {
            let _ = write!(key, "i{}", bi.value);
            true
        }
        // member/computed/call/new/this/template/sequence/... disqualified.
        _ => false,
    }
}

/// Evaluation / side-effect context: same conservative knobs as folding, peephole,
/// DCE, CSE. Property reads + unknown globals assumed effectful.
struct LicmCtx<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> GlobalContext<'a> for LicmCtx<'a> {
    fn is_global_reference(&self, _reference: &IdentifierReference<'a>) -> bool {
        false
    }
}

impl<'a> MayHaveSideEffectsContext<'a> for LicmCtx<'a> {
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

impl<'a> ConstantEvaluationCtx<'a> for LicmCtx<'a> {
    fn ast(&self) -> AstBuilder<'a> {
        self.ast
    }
}
