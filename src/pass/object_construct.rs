//! Pass: object-construction folding (min level O2).
//!
//! Collapses a freshly-declared object that is built up by a run of immediately
//! following own-property stores into a single object literal:
//!
//! ```js
//! var x = {};        //  =>  var x = { y: 1, z: 2 };
//! x.y = 1;
//! x.z = 2;
//! ```
//!
//! This is the JS analogue of "store-to-literal hoisting": the consecutive
//! `x.<key> = <rhs>` statements become own data properties of the declarator's
//! object literal, in source order. It composes with constant-folding / DCE
//! (the single literal is a cleaner optimization target than scattered stores).
//!
//! SOUNDNESS. The key hazard is that `x.k = v` is a `[[Set]]` (which consults an
//! inherited setter / non-writable data property on the prototype) while a
//! literal property `{k: v}` is a `[[DefineOwnProperty]]` (CreateDataProperty,
//! which IGNORES the prototype). They diverge only when the receiver inherits an
//! ACCESSOR or NON-WRITABLE data property for that key. The object we fold into is
//! always an object *literal*, so its prototype is the realm's pristine
//! `%Object.prototype%` — whose only accessor is `__proto__` — UNLESS the program
//! taints it. We therefore gate the transform on ALL of:
//!
//!   1. Whole-program bail on `with` / direct `eval` (scope data unreliable), and
//!      on any in-program mutation of `%Object.prototype%` that could install a
//!      divergent accessor / non-writable data prop or change its prototype chain
//!      (`Object.prototype.k = …`, `Object.defineProperty`/`setPrototypeOf`/
//!      `Reflect.*` targeting it, `__defineGetter__`/`__defineSetter__`) — see
//!      [`prototype_possibly_tainted`]. Aliasing `Object.prototype` into a local
//!      first, or a taint from external code, is not detected (the same posture
//!      pure-eval takes toward host globals).
//!   2. The base initializer must be a plain object literal whose existing members
//!      are only spreads and `Init` data properties — no accessor (`get`/`set`,
//!      which a same-key store would diverge against) and no `__proto__:`
//!      prototype-setter (which would give the object a non-standard prototype).
//!   3. Each folded store key must be statically known and not `__proto__` (the
//!      `__proto__` accessor on `Object.prototype`, and the literal `__proto__:`
//!      special form, both differ from a plain own-property define).
//!   4. The target `x` is a single, simple-identifier declarator binding (never an
//!      assignment-form `x = {}`, whose const-reassign throw timing differs), and
//!      the run of stores immediately follows it with nothing observing `x` in
//!      between.
//!   5. THROW-vs-binding hazard. If a store RHS throws partway, the ORIGINAL has
//!      already assigned `x` the (partial) object, while the FOLDED literal never
//!      completes so `x` stays `undefined`/TDZ. This is observable only if a
//!      `catch`/`finally` (or sibling script, for a top-level `var`) can read `x`
//!      after the throw. We therefore allow an effectful / possibly-throwing RHS
//!      ONLY when the fold site is inside a function body with NO enclosing `try`
//!      (a throw unwinds the function and the non-closed-over local `x` is
//!      unobservable) AND `x` is not closed over (no closure could read the
//!      half-built object during an RHS call). Otherwise every folded RHS must be a
//!      literal (never throws, no side effects, never reads `x`).
//!   6. A folded RHS may never reference `x` itself: `let x = {y: x}` is a TDZ
//!      `ReferenceError` and `var x = {y: x}` reads hoisted `undefined`, neither
//!      matching the un-folded `x.y = x` (which reads the live object).
//!
//! IDEMPOTENCE: folding removes the store statements and enriches the literal, so
//! a re-run finds no store following the (now sole) declarator and reports
//! UNCHANGED — the driver fixpoint terminates.

use oxc_allocator::{Allocator, Vec as ArenaVec};
use oxc_ast::ast::{
    Argument, AssignmentOperator, AssignmentTarget, BindingPattern, Expression,
    IdentifierReference, ObjectExpression, ObjectPropertyKind, Program, PropertyKey, PropertyKind,
    Statement, StaticMemberExpression,
};
use oxc_ast::AstBuilder;
use oxc_semantic::Scoping;
use oxc_span::SPAN;
use oxc_syntax::symbol::SymbolId;
use oxc_traverse::{Ancestor, Traverse, TraverseCtx};

use crate::level::OptLevel;
use crate::pass::{run_traverse, Pass, PassConfig, PassResult};
use crate::semantic_util::{program_has_with_or_eval, symbol_is_closed_over};

#[derive(Default)]
pub struct ObjectConstruction;

impl Pass for ObjectConstruction {
    fn name(&self) -> &'static str {
        "object-construction"
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
        // `with` / direct `eval` defeat scope resolution; bail entirely.
        if program_has_with_or_eval(program) {
            return PassResult::UNCHANGED;
        }
        // A mutation of `%Object.prototype%` could install an accessor / non-writable
        // data property that a store's `[[Set]]` would consult but the folded
        // literal's CreateDataProperty would ignore — so we conservatively refuse the
        // whole pass if the program (anywhere — including a function an RHS might
        // call mid-run) mutates `Object.prototype`. Aliasing `Object.prototype` into
        // a local first, or tainting from external code, is not detected (the same
        // posture pure-eval takes toward host globals).
        if prototype_possibly_tainted(program, scoping) {
            return PassResult::UNCHANGED;
        }

        let mut visitor = ObjConstructVisitor { changed: false };
        run_traverse(&mut visitor, allocator, program, scoping, ());
        if visitor.changed {
            PassResult::CHANGED
        } else {
            PassResult::UNCHANGED
        }
    }
}

/// True if the program may mutate `%Object.prototype%` in a way that installs an
/// accessor / non-writable data property (or changes its own prototype chain),
/// which would make a store's `[[Set]]` diverge from the literal's
/// CreateDataProperty. Detected forms (all rooted at the genuine global `Object`):
///   * `Object.prototype.k = …` / `Object.prototype[k] = …` (assignment target),
///   * `Object.defineProperty`/`defineProperties`/`setPrototypeOf(Object.prototype, …)`,
///   * `Reflect.defineProperty`/`set`/`setPrototypeOf(Object.prototype, …)`,
///   * `Object.prototype.__defineGetter__`/`__defineSetter__(…)`.
///
/// A plain `Object.prototype.m = fn` only creates a writable data prop (no
/// divergence), but is still flagged here — a deliberately blunt, sound
/// over-approximation.
fn prototype_possibly_tainted(program: &Program, scoping: &Scoping) -> bool {
    use oxc_ast_visit::Visit;
    struct Taint<'s> {
        scoping: &'s Scoping,
        tainted: bool,
    }
    impl<'s> Taint<'s> {
        /// `e` is the genuine global identifier `name` (resolves to no local symbol).
        fn is_global(&self, e: &Expression, name: &str) -> bool {
            matches!(e, Expression::Identifier(id)
                if id.name == name && resolve_symbol(id, self.scoping).is_none())
        }
        /// `m` is the member expression `Object.prototype` on the global `Object`.
        fn is_object_prototype(&self, m: &StaticMemberExpression) -> bool {
            m.property.name == "prototype" && self.is_global(&m.object, "Object")
        }
        /// `e` is syntactically `Object.prototype`.
        fn expr_is_object_prototype(&self, e: &Expression) -> bool {
            matches!(e, Expression::StaticMemberExpression(m) if self.is_object_prototype(m))
        }
        /// The first call argument is syntactically `Object.prototype`.
        fn first_arg_is_object_prototype(&self, args: &[Argument]) -> bool {
            matches!(args.first(), Some(Argument::StaticMemberExpression(m)) if self.is_object_prototype(m))
        }
    }
    impl<'a, 's> Visit<'a> for Taint<'s> {
        fn visit_assignment_expression(&mut self, it: &oxc_ast::ast::AssignmentExpression<'a>) {
            let obj = match &it.left {
                AssignmentTarget::StaticMemberExpression(m) => Some(&m.object),
                AssignmentTarget::ComputedMemberExpression(m) => Some(&m.object),
                _ => None,
            };
            if obj.is_some_and(|o| self.expr_is_object_prototype(o)) {
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
                if mutates_first_arg && self.first_arg_is_object_prototype(&it.arguments) {
                    self.tainted = true;
                }
                // `Object.prototype.__defineGetter__/__defineSetter__(...)`.
                if matches!(method, "__defineGetter__" | "__defineSetter__")
                    && self.expr_is_object_prototype(&callee.object)
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

struct ObjConstructVisitor {
    changed: bool,
}

impl<'a> Traverse<'a, ()> for ObjConstructVisitor {
    /// Statement-list transform: rewrite each `<decl x = {…}>` followed by a run of
    /// foldable `x.k = v` stores into a single enriched declarator. Runs post-order
    /// so inner lists are already folded; nested folds don't interact (each is
    /// confined to its own statement list).
    fn exit_statements(
        &mut self,
        node: &mut ArenaVec<'a, Statement<'a>>,
        ctx: &mut TraverseCtx<'a, ()>,
    ) {
        // Cheap pre-check: a fold needs a declarator immediately followed by a
        // member-store expression statement. Skip the drain/rebuild otherwise so we
        // never churn (and never spuriously report CHANGED).
        if !has_fold_candidate(node) {
            return;
        }

        // `allow_unsafe`: may we fold a store whose RHS could throw / have side
        // effects? Only when a throw at the fold site is unobservable — inside a
        // function body with no enclosing `try` (see hazard #5). Computed before we
        // borrow scoping; `ctx.ast` is a Copy so it holds no borrow on `ctx`.
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
            // Take the declarator head and append each store's property to its
            // object literal, in order.
            let mut head = items[i].take().expect("present");
            {
                let props = head_object_props_mut(&mut head).expect("planned head has object init");
                for slot in items.iter_mut().skip(i + 1).take(fold_count) {
                    let store = slot.take().expect("present");
                    props.push(store_to_property(store, ast));
                }
            }
            out.push(head);
            self.changed = true;
            i += fold_count + 1;
        }

        *node = out;
    }
}

/// Cheap structural pre-check: is there an index where a (single, object-init)
/// `var`/`let`/`const` declarator is immediately followed by a member-store
/// expression statement? Avoids the drain/rebuild allocation in the common case.
fn has_fold_candidate(node: &[Statement]) -> bool {
    node.windows(2)
        .any(|w| statement_is_object_decl_head(&w[0]) && statement_is_member_store(&w[1]))
}

/// True if `stmt` is a single-declarator `var`/`let`/`const` whose binding is a
/// simple identifier and whose initializer is an object literal (shape check only;
/// eligibility of the literal's members is verified in `plan_fold`).
fn statement_is_object_decl_head(stmt: &Statement) -> bool {
    let Statement::VariableDeclaration(vd) = stmt else {
        return false;
    };
    if vd.declarations.len() != 1 {
        return false;
    }
    let d = &vd.declarations[0];
    matches!(d.id, BindingPattern::BindingIdentifier(_))
        && matches!(&d.init, Some(Expression::ObjectExpression(_)))
}

/// True if `stmt` is an expression statement whose expression is a plain `=`
/// assignment to a static/computed member (shape only).
fn statement_is_member_store(stmt: &Statement) -> bool {
    let Statement::ExpressionStatement(es) = stmt else {
        return false;
    };
    let Expression::AssignmentExpression(a) = &es.expression else {
        return false;
    };
    a.operator == AssignmentOperator::Assign
        && matches!(
            a.left,
            AssignmentTarget::StaticMemberExpression(_)
                | AssignmentTarget::ComputedMemberExpression(_)
        )
}

/// Decide how many statements after `items[idx]` can be folded into the head
/// declarator's object literal. Returns 0 if `items[idx]` is not an eligible head
/// or no following store qualifies.
fn plan_fold(
    items: &[Option<Statement>],
    idx: usize,
    scoping: &Scoping,
    allow_unsafe: bool,
) -> usize {
    let Some(head) = items[idx].as_ref() else {
        return 0;
    };
    let Some(sym) = eligible_head_symbol(head) else {
        return 0;
    };
    // An effectful/throwing RHS is only safe when the throw is unobservable AND no
    // closure could read the half-built object during an RHS call.
    let rhs_must_be_literal = !allow_unsafe || symbol_is_closed_over(sym, scoping);

    let mut count = 0;
    let mut j = idx + 1;
    while j < items.len() {
        let Some(stmt) = items[j].as_ref() else {
            break;
        };
        if !store_is_foldable(stmt, sym, scoping, rhs_must_be_literal) {
            break;
        }
        count += 1;
        j += 1;
    }
    count
}

/// If `head` is an eligible declarator (single simple-identifier binding, object
/// literal initializer whose members are all spreads / `Init` data properties and
/// contain no `__proto__:` prototype setter), return its bound symbol.
fn eligible_head_symbol(head: &Statement) -> Option<SymbolId> {
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
    let Some(Expression::ObjectExpression(obj)) = &d.init else {
        return None;
    };
    if !base_literal_eligible(obj) {
        return None;
    }
    Some(sym)
}

/// The base object literal may only contain spreads and plain `Init` data
/// properties, and no `__proto__:` prototype setter. An accessor (`get`/`set`)
/// would diverge against a same-key store (the store invokes the setter; the
/// folded literal redefines the key as data), and a `__proto__:` setter gives the
/// object a non-standard prototype the stores' `[[Set]]` would consult.
fn base_literal_eligible(obj: &ObjectExpression) -> bool {
    obj.properties.iter().all(|p| match p {
        ObjectPropertyKind::SpreadProperty(_) => true,
        ObjectPropertyKind::ObjectProperty(op) => {
            op.kind == PropertyKind::Init && !is_proto_setter(op.computed, op.shorthand, &op.key)
        }
    })
}

/// The literal `__proto__: value` prototype-setter form: a non-computed,
/// non-shorthand `Init` property whose key is the identifier/string `__proto__`.
/// (`{["__proto__"]: v}`, `{__proto__}`, and `__proto__(){}` are ordinary own
/// properties and are NOT prototype setters.)
fn is_proto_setter(computed: bool, shorthand: bool, key: &PropertyKey) -> bool {
    !computed && !shorthand && property_key_string(key).as_deref() == Some("__proto__")
}

/// Static string form of a property key, for `__proto__` detection.
fn property_key_string(key: &PropertyKey) -> Option<String> {
    match key {
        PropertyKey::StaticIdentifier(id) => Some(id.name.to_string()),
        PropertyKey::StringLiteral(s) => Some(s.value.to_string()),
        _ => None,
    }
}

/// True if `stmt` is a foldable store `x.<key> = <rhs>` for symbol `sym`:
///   * a plain `=` assignment whose target is a non-optional static member
///     (`x.k`) or computed member with a string/numeric literal key (`x["k"]` /
///     `x[0]`), whose object resolves to `sym`;
///   * the key is not `__proto__`;
///   * the RHS is a literal when `rhs_must_be_literal`, else any expression that
///     does not reference `sym`.
fn store_is_foldable(
    stmt: &Statement,
    sym: SymbolId,
    scoping: &Scoping,
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

    let key = match &a.left {
        AssignmentTarget::StaticMemberExpression(m) => {
            if m.optional || !object_is_symbol(&m.object, sym, scoping) {
                return false;
            }
            m.property.name.to_string()
        }
        AssignmentTarget::ComputedMemberExpression(m) => {
            if m.optional || !object_is_symbol(&m.object, sym, scoping) {
                return false;
            }
            match computed_literal_key(&m.expression) {
                Some(k) => k,
                None => return false,
            }
        }
        _ => return false,
    };
    if key == "__proto__" {
        return false;
    }

    if rhs_must_be_literal {
        is_literal_expr(&a.right)
    } else {
        !expr_references_symbol(&a.right, sym, scoping)
    }
}

/// True if `expr` is an identifier reference resolving to `sym`.
fn object_is_symbol(expr: &Expression, sym: SymbolId, scoping: &Scoping) -> bool {
    let Expression::Identifier(id) = expr else {
        return false;
    };
    resolve_symbol(id, scoping) == Some(sym)
}

/// A computed member key we can rebuild as a literal property key (string or
/// numeric literal). Other computed keys (identifiers, exprs) are rejected: their
/// runtime value is unknown (could be `__proto__`) and could read `sym`.
fn computed_literal_key(expr: &Expression) -> Option<String> {
    match expr {
        Expression::StringLiteral(s) => Some(s.value.to_string()),
        Expression::NumericLiteral(n) => Some(n.value.to_string()),
        _ => None,
    }
}

/// The literal expression kinds: never throw, never have side effects, and never
/// reference another binding — so they are always safe to fold.
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

/// True if any identifier reference within `expr` resolves to `sym`.
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

/// Mutable handle to the head declarator's object-literal properties vec.
fn head_object_props_mut<'b, 'a>(
    head: &'b mut Statement<'a>,
) -> Option<&'b mut ArenaVec<'a, ObjectPropertyKind<'a>>> {
    let Statement::VariableDeclaration(vd) = head else {
        return None;
    };
    let d = vd.declarations.first_mut()?;
    let Some(Expression::ObjectExpression(obj)) = &mut d.init else {
        return None;
    };
    Some(&mut obj.properties)
}

/// Convert a foldable store statement (validated by `store_is_foldable`) into an
/// `Init` object property. Consumes the statement, moving its RHS as the value.
fn store_to_property<'a>(store: Statement<'a>, ast: AstBuilder<'a>) -> ObjectPropertyKind<'a> {
    let Statement::ExpressionStatement(es) = store else {
        unreachable!("planned store is an expression statement");
    };
    let Expression::AssignmentExpression(a) = es.unbox().expression else {
        unreachable!("planned store is an assignment");
    };
    let a = a.unbox();
    let value = a.right;
    let (key, computed) = match a.left {
        AssignmentTarget::StaticMemberExpression(m) => {
            let prop = m.unbox().property;
            (
                ast.property_key_static_identifier(prop.span, prop.name),
                false,
            )
        }
        AssignmentTarget::ComputedMemberExpression(m) => {
            let key_expr = m.unbox().expression;
            let key = match key_expr {
                Expression::StringLiteral(s) => PropertyKey::StringLiteral(s),
                Expression::NumericLiteral(n) => PropertyKey::NumericLiteral(n),
                _ => unreachable!("planned computed key is a string/numeric literal"),
            };
            (key, true)
        }
        _ => unreachable!("planned store target is a member expression"),
    };
    ast.object_property_kind_object_property(
        SPAN,
        PropertyKind::Init,
        key,
        value,
        false,
        false,
        computed,
    )
}

/// Whether a throw at this statement-list fold site is observable on the binding.
///
/// Walking ancestors outward: an enclosing `try` block/handler/finalizer makes a
/// throw catchable (observable on `x`) → true. Reaching a function body / static
/// block first means the throw unwinds the function and the local `x` is
/// unobservable → false. Reaching the program root (top-level: a `var` is a global
/// observable by sibling scripts) is treated conservatively as observable → true.
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
