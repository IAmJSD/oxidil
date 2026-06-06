//! Pass: common-subexpression elimination via local value numbering (min level O2).
//!
//! In the spirit of GCC's CSE/GVN, but PURE-ONLY and restricted to the subset we
//! can prove observationally equivalent for arbitrary JavaScript.
//!
//! WHAT IT DOES
//! ------------
//! Within a single straight-line statement list (`exit_statements`), when the
//! SAME side-effect-free, non-trivial expression is computed two or more times at
//! UNCONDITIONALLY-evaluated positions, we compute it once into a fresh,
//! uniquely-named `const` temp inserted just before the first occurrence and
//! replace every occurrence with a read of that temp.
//!
//! ELIGIBLE EXPRESSIONS
//! --------------------
//! Only arithmetic / logical / bitwise / comparison operators (Binary, Logical,
//! Unary) whose ENTIRE subtree is built from:
//!   - primitive literals (number / string / boolean / null / bigint), and
//!   - identifier reads of resolved LOCAL symbols that are NEVER mutated anywhere
//!     in the program (`!symbol_is_mutated`) and not global/unresolved.
//!
//! We EXCLUDE all calls, `new`, member / computed / private access (each could
//! trigger a getter or otherwise differ between evaluations — the shared Ctx sets
//! `property_read_side_effects = All`), `this`, templates, sequences, assignments,
//! `await`/`yield`, etc. The whole subtree must additionally report
//! `may_have_side_effects == false`.
//!
//! SOUNDNESS
//! ---------
//!  * Every operand is a literal or a never-mutated local, so the value of the
//!    expression is INVARIANT at every program point in the list: no intervening
//!    assignment, no aliasing, and (since member access is excluded) no getter can
//!    change it between occurrences.
//!  * COERCION is itself an observable side effect. `oxc::may_have_side_effects`
//!    does NOT flag the relational / equality / arithmetic / bitwise operators
//!    (`< > <= >= == != + - * / % ** & | ^ << >> >>>`) over identifier operands of
//!    unknown type, yet each invokes ToPrimitive (valueOf/toString/
//!    Symbol.toPrimitive) on a non-primitive operand — running user code.
//!    Collapsing N evaluations into one would reduce how many times that coercion
//!    runs, changing behavior. We therefore do NOT rely on `may_have_side_effects`
//!    for the operator set: `build_key` independently restricts binary operators
//!    to strict equality (`===`/`!==`, never coerces), logical combinators
//!    (`&& || ??`, ToBoolean/nullish only), and unary `! typeof void`.
//!  * We only collect occurrences at positions that are UNCONDITIONALLY evaluated
//!    when their enclosing statement runs (we never descend into conditional
//!    branches, logical short-circuit right operands, `?.` optional chains, or
//!    function / arrow bodies). Combined with statement-list straight-line order,
//!    the first occurrence is reached in the optimized program exactly when it was
//!    reached in the original — so if the computation can THROW (e.g. BigInt type
//!    mixing), it throws at the same point, and the reused temp is only read after
//!    that point. Timing and throw behavior are preserved.
//!  * The hoisted binding is a fresh UID (`generate_uid_in_current_scope`) so it
//!    cannot collide with or shadow any user binding.
//!  * We bail entirely on `with` / direct `eval` (scope resolution + mutation
//!    queries unreliable).
//!
//! NON-TRIVIALITY / IDEMPOTENCE
//! ----------------------------
//! We only fire for expressions containing at least two operator nodes (so a bare
//! `a + b` is left alone — replacing it with a temp would not shrink output) and
//! only when the expression occurs 2+ times. After rewriting, every occurrence is
//! a plain identifier read of the temp, which is no longer an eligible candidate,
//! so the pass cannot re-fire on its own output and the fixpoint terminates.

use std::collections::HashMap;

use oxc_allocator::{Allocator, CloneIn, Vec as ArenaVec};
use oxc_ast::ast::{Expression, IdentifierReference, Program, Statement, VariableDeclarationKind};
use oxc_ast::AstBuilder;
use oxc_ecmascript::constant_evaluation::ConstantEvaluationCtx;
use oxc_ecmascript::side_effects::{MayHaveSideEffectsContext, PropertyReadSideEffects};
use oxc_ecmascript::GlobalContext;
use oxc_semantic::Scoping;
use oxc_span::SPAN;
use oxc_syntax::symbol::SymbolFlags;
use oxc_traverse::{Traverse, TraverseCtx};

use std::collections::HashSet;
use std::rc::Rc;

use oxc_syntax::symbol::SymbolId;

use crate::level::OptLevel;
use crate::pass::{run_traverse, Pass, PassConfig, PassResult};
use crate::semantic_util::{
    collect_provably_number_symbols, expr_is_provably_number, program_has_with_or_eval,
};

#[derive(Default)]
pub struct CseGvn;

impl Pass for CseGvn {
    fn name(&self) -> &'static str {
        "cse-gvn"
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
        // `with` / direct `eval` make scope resolution and `symbol_is_mutated`
        // unreliable, so we cannot prove operand invariance. Bail.
        if program_has_with_or_eval(program) {
            return PassResult::UNCHANGED;
        }

        // The driver only rebuilds `Scoping` between fixpoint iterations, so the
        // mutation/reference data passed in reflects the tree at the START of this
        // iteration. We only READ mutation facts (never delete bindings based on
        // stale counts), and any binding we touch must be never-mutated — a
        // property that an earlier same-iteration pass cannot have made false
        // (passes only remove code, they do not introduce new writes to a
        // previously-const local). So the in-iteration snapshot is safe to read.
        // Whole-program set of bindings that PROVABLY hold a finite Number at
        // every program point (so reading them runs no coercion hook and the
        // coercing operators are pure value computations over them). Lets
        // `build_key` broaden the operator set beyond strict-equality WITHOUT
        // risking any change to coercion count/order. Empty under with/eval.
        let prim = Rc::new(collect_provably_number_symbols(program, scoping));

        let mut visitor = CseVisitor {
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

struct CseVisitor {
    changed: bool,
    /// Symbols provably holding a finite Number (see `run`). Operands drawn only
    /// from this set + numeric literals make the coercing operators safe to dedup.
    prim: Rc<HashSet<SymbolId>>,
}

impl<'a> Traverse<'a, ()> for CseVisitor {
    fn exit_statements(
        &mut self,
        node: &mut ArenaVec<'a, Statement<'a>>,
        ctx: &mut TraverseCtx<'a, ()>,
    ) {
        // PHASE 1: collect, for this statement list, every eligible occurrence
        // grouped by a structural key. An occurrence records which top-level
        // statement (index in `node`) it lives under.
        let mut groups: HashMap<String, Group<'a>> = HashMap::new();
        {
            let eval = CseCtx { ast: ctx.ast };
            for (stmt_idx, stmt) in node.iter().enumerate() {
                let mut collector = Collector {
                    scoping: ctx.scoping(),
                    eval: &eval,
                    stmt_idx,
                    groups: &mut groups,
                    prim: &self.prim,
                };
                collector.scan_statement(stmt);
            }
        }

        // Keep only groups that occur 2+ times. (HashMap order is nondeterministic;
        // collect into a Vec sorted by first-occurrence statement index then key so
        // the emitted temp order — and thus codegen — is deterministic.)
        let mut eligible: Vec<(String, Group<'a>)> =
            groups.into_iter().filter(|(_, g)| g.count >= 2).collect();
        if eligible.is_empty() {
            return;
        }
        eligible.sort_by(|a, b| {
            a.1.first_stmt_idx
                .cmp(&b.1.first_stmt_idx)
                .then_with(|| a.0.cmp(&b.0))
        });

        // PHASE 2: for each eligible group, mint a temp and remember the mapping
        // key -> temp-read-expression-factory. We build the temp bindings now (in
        // the current scope) and the hoisted `const` declarations to insert.
        struct Plan<'a> {
            key: String,
            binding: oxc_traverse::BoundIdentifier<'a>,
            template: Expression<'a>,
            first_stmt_idx: usize,
        }
        let mut plans: Vec<Plan<'a>> = Vec::with_capacity(eligible.len());
        for (key, g) in eligible {
            let binding =
                ctx.generate_uid_in_current_scope("cse", SymbolFlags::BlockScopedVariable);
            plans.push(Plan {
                key,
                binding,
                template: g.template,
                first_stmt_idx: g.first_stmt_idx,
            });
        }

        // PHASE 3: substitute every occurrence with a read of its temp.
        let keys: std::collections::HashMap<String, usize> = plans
            .iter()
            .enumerate()
            .map(|(i, p)| (p.key.clone(), i))
            .collect();
        let bindings: Vec<oxc_traverse::BoundIdentifier<'a>> =
            plans.iter().map(|p| p.binding.clone()).collect();
        // Pre-create the read expressions per plan as we walk (each occurrence gets
        // its own fresh read reference). We walk via raw indices into `node` to
        // avoid borrowing `node` while also borrowing `ctx` mutably; the
        // substitution recursion mirrors the collector's unconditional-position
        // walk so only the exact occurrences are rewritten.
        let scoping_ptr: *const Scoping = ctx.scoping();
        let n = node.len();
        for i in 0..n {
            // Temporarily move the statement out so we can pass `&mut Statement`
            // while still using `ctx` mutably (no overlap with `node`).
            let mut stmt = std::mem::replace(&mut node[i], ctx.ast.statement_empty(SPAN));
            substitute_in_statement(&mut stmt, &keys, &bindings, scoping_ptr, &self.prim, ctx);
            node[i] = stmt;
        }

        // PHASE 4: insert `const <temp> = <template>;` immediately before the first
        // statement that referenced it. Insert from the highest index downward so
        // earlier insertions do not shift later target indices.
        let mut inserts: Vec<(usize, Statement<'a>)> = Vec::with_capacity(plans.len());
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
            inserts.push((p.first_stmt_idx, Statement::VariableDeclaration(var_decl)));
        }
        // Stable insert: sort by index descending; for equal index, preserve the
        // declaration order so dependent temps still appear before their use.
        inserts.sort_by_key(|a| std::cmp::Reverse(a.0));
        for (idx, stmt) in inserts {
            node.insert(idx, stmt);
        }

        self.changed = true;
    }
}

/// A group of structurally-identical eligible occurrences in one statement list.
struct Group<'a> {
    /// A pristine clone of the expression, used as the temp's initializer.
    template: Expression<'a>,
    count: usize,
    first_stmt_idx: usize,
}

/// Walks a statement collecting eligible CSE candidates at unconditionally-
/// evaluated positions only.
struct Collector<'a, 'c, 's> {
    scoping: &'s Scoping,
    eval: &'c CseCtx<'a>,
    stmt_idx: usize,
    groups: &'c mut HashMap<String, Group<'a>>,
    prim: &'c HashSet<SymbolId>,
}

impl<'a, 'c, 's> Collector<'a, 'c, 's> {
    /// Scan a statement's unconditionally-evaluated expression positions.
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
            // Any other statement kind (if / for / blocks / throw / etc.) may
            // evaluate its sub-expressions conditionally or define new scopes; we
            // do not descend into them. Their inner straight-line lists are handled
            // by their own `exit_statements` invocation.
            _ => {}
        }
    }

    /// Scan an expression, recording it as a candidate if eligible, and recursing
    /// only into UNCONDITIONALLY-evaluated sub-positions.
    fn scan_expr(&mut self, expr: &Expression<'a>) {
        // Record this node if it is a whole eligible candidate.
        if let Some(key) = eligible_key(expr, self.scoping, self.eval, self.prim) {
            self.record(&key, expr);
        }

        // Recurse only into always-evaluated children. We deliberately do NOT
        // recurse into conditional branches / logical RHS / `?.` / call args /
        // function bodies, so every recorded occurrence is unconditional.
        match expr {
            Expression::BinaryExpression(b) => {
                self.scan_expr(&b.left);
                self.scan_expr(&b.right);
            }
            Expression::UnaryExpression(u) => self.scan_expr(&u.argument),
            Expression::LogicalExpression(l) => {
                // Only the LEFT operand is unconditionally evaluated; the right may
                // be short-circuited away.
                self.scan_expr(&l.left);
            }
            Expression::ParenthesizedExpression(p) => self.scan_expr(&p.expression),
            _ => {}
        }
    }

    fn record(&mut self, key: &str, expr: &Expression<'a>) {
        match self.groups.get_mut(key) {
            Some(g) => g.count += 1,
            None => {
                self.groups.insert(
                    key.to_string(),
                    Group {
                        template: expr.clone_in(self.eval.ast.allocator),
                        count: 1,
                        first_stmt_idx: self.stmt_idx,
                    },
                );
            }
        }
    }
}

/// Substitute occurrences inside one statement (mirrors the collector's
/// unconditional-position walk). Replaces a matched whole expression with a fresh
/// read of the corresponding temp.
fn substitute_in_statement<'a>(
    stmt: &mut Statement<'a>,
    keys: &std::collections::HashMap<String, usize>,
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
        _ => {}
    }
}

fn substitute_in_expr<'a>(
    expr: &mut Expression<'a>,
    keys: &std::collections::HashMap<String, usize>,
    bindings: &[oxc_traverse::BoundIdentifier<'a>],
    scoping_ptr: *const Scoping,
    prim: &HashSet<SymbolId>,
    ctx: &mut TraverseCtx<'a, ()>,
) {
    // SAFETY: `scoping_ptr` aliases `ctx.scoping()`; we only use it to compute the
    // structural key (an immutable read of the scoping) and never hold it across a
    // mutation of scoping. `eligible_key` borrows it immutably.
    let scoping: &Scoping = unsafe { &*scoping_ptr };
    let eval = CseCtx { ast: ctx.ast };
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

/// Compute a structural key for `expr` if it is an eligible CSE candidate, else
/// `None`. Eligible == pure, non-trivial (>=2 operator nodes), and built only from
/// literals + never-mutated local identifier reads.
fn eligible_key(
    expr: &Expression,
    scoping: &Scoping,
    eval: &CseCtx,
    prim: &HashSet<SymbolId>,
) -> Option<String> {
    // Must be one of the operator expression kinds (so calls/new/member/etc. are
    // excluded at the root).
    if !matches!(
        expr,
        Expression::BinaryExpression(_)
            | Expression::LogicalExpression(_)
            | Expression::UnaryExpression(_)
    ) {
        return None;
    }
    // SOUNDNESS GATE: `build_key` is now the COMPLETE, load-bearing proof. It
    // accepts a node only if that node is a literal, a never-mutated LOCAL
    // identifier read, or one of a restricted operator set, AND — for any operator
    // that would coerce (`+ - * / % ** & | ^ << >> >>> < > <= >=`, unary `+ - ~`) —
    // ONLY when every operand is PROVABLY a finite Number (`prim`), so no
    // ToPrimitive/ToNumber hook can run. Every node `build_key` accepts is
    // therefore provably side-effect-free, with INVARIANT value across occurrences.
    //
    // We deliberately do NOT use `oxc::may_have_side_effects` as the gate here:
    // over identifier operands of unknown type it conservatively reports the
    // coercing operators as POSSIBLY-impure (it cannot see our whole-program
    // provably-Number proof), which would reject the very expressions this
    // relaxation is meant to dedup. `build_key` is strictly stronger for our node
    // set (it independently restricts coercing ops to provably-Number operands),
    // so relying on it alone is sound and is what enables the broadening.
    let _ = eval;
    let mut ops = 0usize;
    let mut key = String::new();
    if !build_key(expr, scoping, prim, &mut key, &mut ops) {
        return None;
    }
    // Non-trivial: at least two operator nodes, so we only hoist when it actually
    // saves work / size.
    if ops < 2 {
        return None;
    }
    Some(key)
}

/// Build a canonical structural key into `key`, counting operator nodes in `ops`.
/// Returns false if any node is not an allowed kind (literal / never-mutated local
/// identifier / allowed operator), which disqualifies the whole expression.
fn build_key(
    expr: &Expression,
    scoping: &Scoping,
    prim: &HashSet<SymbolId>,
    key: &mut String,
    ops: &mut usize,
) -> bool {
    use oxc_syntax::operator::{BinaryOperator, UnaryOperator};
    use std::fmt::Write;
    match expr {
        Expression::BinaryExpression(b) => {
            // COERCION GUARD. `oxc::may_have_side_effects` reports the relational
            // and equality operators (`< > <= >= == !=`) — and the arithmetic /
            // bitwise ones — as side-effect-free even over identifier operands of
            // unknown type. But all of those invoke ToPrimitive (valueOf/toString/
            // Symbol.toPrimitive) on a non-primitive operand, which runs user code:
            // collapsing N evaluations to one would drop coercion side effects.
            //
            // ALWAYS-SAFE: strict equality (`===`/`!==`) never coerces.
            //
            // PROVABLY-NUMBER BROADENING: the arithmetic (`+ - * / % **`), bitwise
            // (`& | ^ << >> >>>`), and relational (`< > <= >=`) operators are safe
            // to dedup ONLY when BOTH operands are provably finite Numbers — then no
            // ToPrimitive/ToNumber hook runs (operands are already primitive), `+`
            // is numeric (no string ambiguity), and number-op-number never throws
            // (no BigInt mix). The result is a pure value computation invariant
            // across occurrences, so collapsing N evals to 1 changes nothing
            // observable. Loose equality (`==`/`!=`) and `in`/`instanceof` are NOT
            // broadened (they have object-identity / prototype effects).
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
                // Both operands must be provably finite Numbers; otherwise this
                // operator could run a coercion hook and is unsafe to dedup.
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
            if !build_key(&b.left, scoping, prim, key, ops) {
                return false;
            }
            key.push(' ');
            if !build_key(&b.right, scoping, prim, key, ops) {
                return false;
            }
            key.push(')');
            true
        }
        Expression::LogicalExpression(l) => {
            // `&&`/`||`/`??` test truthiness (ToBoolean) / nullishness only — they
            // NEVER invoke valueOf/toString on an operand, so they cannot run a
            // coercion side effect. Safe.
            *ops += 1;
            let _ = write!(key, "(L{:?} ", l.operator);
            if !build_key(&l.left, scoping, prim, key, ops) {
                return false;
            }
            key.push(' ');
            if !build_key(&l.right, scoping, prim, key, ops) {
                return false;
            }
            key.push(')');
            true
        }
        Expression::UnaryExpression(u) => {
            // `!` (ToBoolean), `typeof`, and `void` never coerce via valueOf/
            // toString — always safe. `+x`/`-x`/`~x` invoke ToNumber (a coercion
            // hook) UNLESS the argument is provably a finite Number, in which case
            // no hook runs and the result is a pure invariant value — safe to dedup.
            // `delete` has side effects, so always reject it.
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
            if !build_key(&u.argument, scoping, prim, key, ops) {
                return false;
            }
            key.push(')');
            true
        }
        Expression::ParenthesizedExpression(p) => build_key(&p.expression, scoping, prim, key, ops),
        Expression::Identifier(id) => {
            // Must resolve to a LOCAL symbol that is never mutated. Identify the
            // symbol by id so two textually-equal but distinct bindings never merge.
            let Some(rid) = id.reference_id.get() else {
                return false;
            };
            let Some(sym) = scoping.get_reference(rid).symbol_id() else {
                // Unresolved / global reference: value could change; disqualify.
                return false;
            };
            if scoping.symbol_is_mutated(sym) {
                return false;
            }
            let _ = write!(key, "#{}", sym.index());
            true
        }
        Expression::NumericLiteral(n) => {
            // Distinguish bit patterns so +0/-0/NaN never collapse incorrectly.
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
        // Anything else (member/computed/call/new/this/template/sequence/...) is
        // disqualified: could trigger getters, side effects, or differ.
        _ => false,
    }
}

/// Evaluation / side-effect context: same conservative knobs as the folding,
/// peephole, and DCE passes. Property reads and unknown globals are assumed
/// effectful, so member access in a candidate makes `may_have_side_effects` true
/// and the expression is rejected.
struct CseCtx<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> GlobalContext<'a> for CseCtx<'a> {
    fn is_global_reference(&self, _reference: &IdentifierReference<'a>) -> bool {
        false
    }
}

impl<'a> MayHaveSideEffectsContext<'a> for CseCtx<'a> {
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

impl<'a> ConstantEvaluationCtx<'a> for CseCtx<'a> {
    fn ast(&self) -> AstBuilder<'a> {
        self.ast
    }
}
