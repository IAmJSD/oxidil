//! Pass: array-construction folding (min level O2).
//!
//! The array analogue of [`super::object_construct`]: a freshly-declared array
//! built up by a run of ascending, contiguous indexed stores folds into one array
//! literal:
//!
//! ```js
//! var x = [];        //  =>  var x = [1, 2];
//! x[0] = 1;
//! x[1] = 2;
//! ```
//!
//! Only a *dense, ascending, contiguous* run is folded — each store must write the
//! next index (`base_len`, `base_len + 1`, …). This is what makes the rewrite
//! sound:
//!   * an array literal evaluates its elements left-to-right (index 0 first), which
//!     matches the source order of ascending stores, so side-effect order is
//!     preserved (an out-of-order `x[1] = a(); x[0] = b();` would reorder `a`/`b`);
//!   * the result is dense with the same `length` — no hole is introduced (a
//!     sparse `x[0] = 1; x[2] = 3;` would need an elision `[1, , 3]`, which we do
//!     not synthesize, so the run simply stops at the gap);
//!   * folding only *appends* (never overwrites an existing index), so no element
//!     expression is dropped (`[a, b]; x[1] = c;` must keep evaluating `b`).
//!
//! The remaining hazards mirror the object pass (see its module docs):
//!   1. Whole-program bail on `with`/`eval` and on any in-program mutation of
//!      `%Array.prototype%` OR `%Object.prototype%` — an indexed store is a
//!      `[[Set]]` that walks `x → Array.prototype → Object.prototype` for an
//!      inherited accessor / non-writable data prop, which the literal's element
//!      definition (CreateDataProperty) would ignore. See [`prototype_possibly_tainted`].
//!   2. The base must be a *dense* array literal (no spread, no elision/hole) so its
//!      length is statically known and the next index is well-defined.
//!   3. The store key must be a numeric literal that is a canonical array index
//!      (`0 ≤ i < 2^32-1`) equal to the next expected index.
//!   4. A single, simple-identifier `var`/`let`/`const` declarator head (never an
//!      assignment-form `x = []`), with the run immediately following it.
//!   5. THROW-vs-binding: an effectful / possibly-throwing RHS is folded only when
//!      a throw is unobservable — inside a function body with no enclosing `try`
//!      and the binding not closed over; otherwise every folded RHS must be a
//!      literal.
//!   6. A folded RHS may never reference the binding (`var x = [x]` reads hoisted
//!      `undefined`/TDZ, not the live array).
//!
//! IDEMPOTENCE: the run is consumed (stores removed, elements appended), so a
//! re-run finds no indexed store following the now-sole declarator.

use oxc_allocator::{Allocator, Vec as ArenaVec};
use oxc_ast::ast::{
    Argument, ArrayExpression, ArrayExpressionElement, AssignmentOperator, AssignmentTarget,
    BindingPattern, Expression, IdentifierReference, Program, Statement, StaticMemberExpression,
};
use oxc_semantic::Scoping;
use oxc_syntax::symbol::SymbolId;
use oxc_traverse::{Ancestor, Traverse, TraverseCtx};

use crate::level::OptLevel;
use crate::pass::{run_traverse, Pass, PassConfig, PassResult};
use crate::semantic_util::{program_has_with_or_eval, symbol_is_closed_over};

#[derive(Default)]
pub struct ArrayConstruction;

impl Pass for ArrayConstruction {
    fn name(&self) -> &'static str {
        "array-construction"
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
        if program_has_with_or_eval(program) {
            return PassResult::UNCHANGED;
        }
        // An indexed `[[Set]]` consults `Array.prototype` then `Object.prototype`;
        // a mutation of either could install a divergent accessor / non-writable
        // data prop at a numeric key, so bail the whole pass if the program touches
        // either prototype.
        if prototype_possibly_tainted(program, scoping) {
            return PassResult::UNCHANGED;
        }

        let mut visitor = ArrConstructVisitor { changed: false };
        run_traverse(&mut visitor, allocator, program, scoping, ());
        if visitor.changed {
            PassResult::CHANGED
        } else {
            PassResult::UNCHANGED
        }
    }
}

/// True if the program may mutate `%Array.prototype%` or `%Object.prototype%` in a
/// way that installs an accessor / non-writable data property (or changes its
/// prototype chain). Detected forms, all rooted at the genuine global `Array` /
/// `Object`:
///   * `Array.prototype[i] = …` / `Object.prototype.k = …` (assignment target),
///   * `Object.defineProperty`/`defineProperties`/`setPrototypeOf(<proto>, …)`,
///   * `Reflect.defineProperty`/`set`/`setPrototypeOf(<proto>, …)`,
///   * `<proto>.__defineGetter__`/`__defineSetter__(…)`.
///
/// A plain `Array.prototype.m = fn` only creates a writable data prop (no
/// divergence) but is still flagged — a deliberately blunt, sound
/// over-approximation. Aliasing a prototype into a local first, or a taint from
/// external code, is not detected (the same posture pure-eval takes).
fn prototype_possibly_tainted(program: &Program, scoping: &Scoping) -> bool {
    use oxc_ast_visit::Visit;
    struct Taint<'s> {
        scoping: &'s Scoping,
        tainted: bool,
    }
    impl<'s> Taint<'s> {
        fn is_global(&self, e: &Expression, name: &str) -> bool {
            matches!(e, Expression::Identifier(id)
                if id.name == name && resolve_symbol(id, self.scoping).is_none())
        }
        /// `m` is `Array.prototype` or `Object.prototype` on the matching global.
        fn is_builtin_prototype(&self, m: &StaticMemberExpression) -> bool {
            m.property.name == "prototype"
                && (self.is_global(&m.object, "Array") || self.is_global(&m.object, "Object"))
        }
        fn expr_is_builtin_prototype(&self, e: &Expression) -> bool {
            matches!(e, Expression::StaticMemberExpression(m) if self.is_builtin_prototype(m))
        }
        fn first_arg_is_builtin_prototype(&self, args: &[Argument]) -> bool {
            matches!(args.first(), Some(Argument::StaticMemberExpression(m)) if self.is_builtin_prototype(m))
        }
    }
    impl<'a, 's> Visit<'a> for Taint<'s> {
        fn visit_assignment_expression(&mut self, it: &oxc_ast::ast::AssignmentExpression<'a>) {
            let obj = match &it.left {
                AssignmentTarget::StaticMemberExpression(m) => Some(&m.object),
                AssignmentTarget::ComputedMemberExpression(m) => Some(&m.object),
                _ => None,
            };
            if obj.is_some_and(|o| self.expr_is_builtin_prototype(o)) {
                self.tainted = true;
            }
            oxc_ast_visit::walk::walk_assignment_expression(self, it);
        }
        fn visit_call_expression(&mut self, it: &oxc_ast::ast::CallExpression<'a>) {
            if let Expression::StaticMemberExpression(callee) = &it.callee {
                let method = callee.property.name.as_str();
                let mutates_first_arg = (self.is_global(&callee.object, "Object")
                    && matches!(
                        method,
                        "defineProperty" | "defineProperties" | "setPrototypeOf"
                    ))
                    || (self.is_global(&callee.object, "Reflect")
                        && matches!(method, "defineProperty" | "set" | "setPrototypeOf"));
                if mutates_first_arg && self.first_arg_is_builtin_prototype(&it.arguments) {
                    self.tainted = true;
                }
                if matches!(method, "__defineGetter__" | "__defineSetter__")
                    && self.expr_is_builtin_prototype(&callee.object)
                {
                    self.tainted = true;
                }
            }
            oxc_ast_visit::walk::walk_call_expression(self, it);
        }
    }
    let mut t = Taint {
        scoping,
        tainted: false,
    };
    t.visit_program(program);
    t.tainted
}

struct ArrConstructVisitor {
    changed: bool,
}

impl<'a> Traverse<'a, ()> for ArrConstructVisitor {
    fn exit_statements(
        &mut self,
        node: &mut ArenaVec<'a, Statement<'a>>,
        ctx: &mut TraverseCtx<'a, ()>,
    ) {
        if !has_fold_candidate(node) {
            return;
        }

        let allow_unsafe = !fold_site_throw_observable(ctx);
        let ast = ctx.ast;
        let scoping = ctx.scoping();

        let drained: std::vec::Vec<Statement<'a>> = node.drain(..).collect();
        let mut items: std::vec::Vec<Option<Statement<'a>>> =
            drained.into_iter().map(Some).collect();
        let mut out = ast.vec_with_capacity(items.len());

        let mut i = 0;
        while i < items.len() {
            let fold_count = plan_fold(&items, i, scoping, allow_unsafe);
            if fold_count == 0 {
                out.push(items[i].take().expect("present"));
                i += 1;
                continue;
            }
            let mut head = items[i].take().expect("present");
            {
                let elements =
                    head_array_elements_mut(&mut head).expect("planned head has array init");
                for slot in items.iter_mut().skip(i + 1).take(fold_count) {
                    let store = slot.take().expect("present");
                    elements.push(store_to_element(store));
                }
            }
            out.push(head);
            self.changed = true;
            i += fold_count + 1;
        }

        *node = out;
    }
}

/// Cheap pre-check: a (single, array-init) declarator immediately followed by an
/// indexed-store expression statement.
fn has_fold_candidate(node: &[Statement]) -> bool {
    node.windows(2)
        .any(|w| statement_is_array_decl_head(&w[0]) && statement_is_indexed_store(&w[1]))
}

fn statement_is_array_decl_head(stmt: &Statement) -> bool {
    let Statement::VariableDeclaration(vd) = stmt else {
        return false;
    };
    if vd.declarations.len() != 1 {
        return false;
    }
    let d = &vd.declarations[0];
    matches!(d.id, BindingPattern::BindingIdentifier(_))
        && matches!(&d.init, Some(Expression::ArrayExpression(_)))
}

fn statement_is_indexed_store(stmt: &Statement) -> bool {
    let Statement::ExpressionStatement(es) = stmt else {
        return false;
    };
    let Expression::AssignmentExpression(a) = &es.expression else {
        return false;
    };
    a.operator == AssignmentOperator::Assign
        && matches!(a.left, AssignmentTarget::ComputedMemberExpression(_))
}

/// Number of statements after `items[idx]` foldable into the head array literal.
fn plan_fold(
    items: &[Option<Statement>],
    idx: usize,
    scoping: &Scoping,
    allow_unsafe: bool,
) -> usize {
    let Some(head) = items[idx].as_ref() else {
        return 0;
    };
    let Some((sym, base_len)) = eligible_head(head) else {
        return 0;
    };
    let rhs_must_be_literal = !allow_unsafe || symbol_is_closed_over(sym, scoping);

    let mut count: u32 = 0;
    let mut j = idx + 1;
    while j < items.len() {
        let Some(stmt) = items[j].as_ref() else {
            break;
        };
        let expected = match base_len.checked_add(count) {
            Some(e) => e,
            None => break,
        };
        if !store_is_foldable(stmt, sym, scoping, expected, rhs_must_be_literal) {
            break;
        }
        count += 1;
        j += 1;
    }
    count as usize
}

/// If `head` is an eligible declarator (single simple-identifier binding, dense
/// array-literal initializer with no spread / hole), return its symbol and the
/// base literal length (the first index a store may append at).
fn eligible_head(head: &Statement) -> Option<(SymbolId, u32)> {
    let Statement::VariableDeclaration(vd) = head else {
        return None;
    };
    if vd.declarations.len() != 1 {
        return None;
    }
    let d = &vd.declarations[0];
    let BindingPattern::BindingIdentifier(id) = &d.id else {
        return None;
    };
    let sym = id.symbol_id.get()?;
    let Some(Expression::ArrayExpression(arr)) = &d.init else {
        return None;
    };
    let base_len = dense_base_len(arr)?;
    Some((sym, base_len))
}

/// The length of a dense array literal (every element present, no spread, no
/// elision/hole), or `None` if it is sparse / has a spread (then the next index is
/// not statically known).
fn dense_base_len(arr: &ArrayExpression) -> Option<u32> {
    for el in &arr.elements {
        match el {
            ArrayExpressionElement::SpreadElement(_) | ArrayExpressionElement::Elision(_) => {
                return None
            }
            _ => {}
        }
    }
    u32::try_from(arr.elements.len()).ok()
}

/// True if `stmt` is a foldable indexed store `x[<expected>] = <rhs>`.
fn store_is_foldable(
    stmt: &Statement,
    sym: SymbolId,
    scoping: &Scoping,
    expected_index: u32,
    rhs_must_be_literal: bool,
) -> bool {
    let Statement::ExpressionStatement(es) = stmt else {
        return false;
    };
    let Expression::AssignmentExpression(a) = &es.expression else {
        return false;
    };
    if a.operator != AssignmentOperator::Assign {
        return false;
    }
    let AssignmentTarget::ComputedMemberExpression(m) = &a.left else {
        return false;
    };
    if m.optional || !object_is_symbol(&m.object, sym, scoping) {
        return false;
    }
    if array_index(&m.expression) != Some(expected_index) {
        return false;
    }

    if rhs_must_be_literal {
        is_literal_expr(&a.right)
    } else {
        !expr_references_symbol(&a.right, sym, scoping)
    }
}

/// A numeric-literal key that is a canonical array index (`0 ≤ i < 2^32 - 1`).
fn array_index(expr: &Expression) -> Option<u32> {
    let Expression::NumericLiteral(n) = expr else {
        return None;
    };
    let v = n.value;
    if v.fract() != 0.0 || !v.is_finite() || v < 0.0 {
        return None;
    }
    // Array index range is [0, 2^32 - 2]; 2^32 - 1 (u32::MAX) is the max length, not
    // a valid index.
    if v >= u32::MAX as f64 {
        return None;
    }
    let i = v as u32;
    (i as f64 == v).then_some(i)
}

fn object_is_symbol(expr: &Expression, sym: SymbolId, scoping: &Scoping) -> bool {
    let Expression::Identifier(id) = expr else {
        return false;
    };
    resolve_symbol(id, scoping) == Some(sym)
}

fn is_literal_expr(e: &Expression) -> bool {
    matches!(
        e,
        Expression::StringLiteral(_)
            | Expression::NumericLiteral(_)
            | Expression::BooleanLiteral(_)
            | Expression::NullLiteral(_)
            | Expression::BigIntLiteral(_)
            | Expression::RegExpLiteral(_)
    )
}

fn resolve_symbol(id: &IdentifierReference, scoping: &Scoping) -> Option<SymbolId> {
    let rid = id.reference_id.get()?;
    scoping.get_reference(rid).symbol_id()
}

fn expr_references_symbol(expr: &Expression, sym: SymbolId, scoping: &Scoping) -> bool {
    use oxc_ast_visit::Visit;
    struct RefScan<'s> {
        scoping: &'s Scoping,
        sym: SymbolId,
        found: bool,
    }
    impl<'a, 's> Visit<'a> for RefScan<'s> {
        fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
            if !self.found && resolve_symbol(it, self.scoping) == Some(self.sym) {
                self.found = true;
            }
        }
    }
    let mut scan = RefScan {
        scoping,
        sym,
        found: false,
    };
    scan.visit_expression(expr);
    scan.found
}

/// Mutable handle to the head declarator's array-literal elements.
fn head_array_elements_mut<'b, 'a>(
    head: &'b mut Statement<'a>,
) -> Option<&'b mut ArenaVec<'a, ArrayExpressionElement<'a>>> {
    let Statement::VariableDeclaration(vd) = head else {
        return None;
    };
    let d = vd.declarations.first_mut()?;
    let Some(Expression::ArrayExpression(arr)) = &mut d.init else {
        return None;
    };
    Some(&mut arr.elements)
}

/// Convert a foldable indexed store (validated by `store_is_foldable`) into an
/// array element, moving its RHS as the value. The index is implied by position.
fn store_to_element(store: Statement) -> ArrayExpressionElement {
    let Statement::ExpressionStatement(es) = store else {
        unreachable!("planned store is an expression statement");
    };
    let Expression::AssignmentExpression(a) = es.unbox().expression else {
        unreachable!("planned store is an assignment");
    };
    ArrayExpressionElement::from(a.unbox().right)
}

/// Whether a throw at this statement-list fold site is observable on the binding
/// (an enclosing `try`, or top-level where a `var` is a global). Mirrors the
/// object pass's analysis.
fn fold_site_throw_observable(ctx: &TraverseCtx<()>) -> bool {
    for anc in ctx.ancestors() {
        if matches!(
            anc,
            Ancestor::TryStatementBlock(_)
                | Ancestor::TryStatementHandler(_)
                | Ancestor::TryStatementFinalizer(_)
        ) {
            return true;
        }
        if anc.is_function_body() || anc.is_static_block() {
            return false;
        }
        if anc.is_program() {
            return true;
        }
    }
    true
}
