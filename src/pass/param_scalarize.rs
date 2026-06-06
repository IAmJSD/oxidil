//! Pass: parameter scalarization (min level O2).
//!
//! Replaces a local function's trailing **options-object** parameter with the
//! individual scalar parameters it decomposes into, and rewrites every call site
//! that passes an object literal to pass those values positionally:
//!
//! ```js
//! function f(opts) { return opts.a + opts.b; }      function f(a, b) { return a + b; }
//! f({ a: 1, b: g() });                          =>  f(1, g());
//! f({ a: x, b: y });                                f(x, y);
//! ```
//!
//! This is scalar-replacement-of-aggregates applied interprocedurally to a
//! non-escaping local function: it removes the per-call object allocation and
//! exposes the values to constant-folding / inlining. It is `-O2`.
//!
//! ## Why it is sound
//!
//! The transform coordinates the function definition with ALL of its call sites,
//! so both the function and the object must be fully analyzable:
//!
//! FUNCTION (must not let itself or `opts` escape):
//!   * A local `function` declaration whose binding is **not exported** and (in a
//!     script) not at the shared global root, and whose every reference is the
//!     **direct callee of a call** (`f(...)`) — never `new f`, `f.bind`, a value,
//!     a tagged template, etc. (checked by `#callee-occurrences == #references`).
//!   * Does not use `arguments` (rewriting the arity would change it).
//!   * The last parameter is a simple identifier `opts` (no default / rest), and
//!     every use of `opts` is a static, non-computed, non-optional `opts.<ident>`
//!     member — as a READ or as the target of `=` / compound-assign / `++`/`--`
//!     (a value mutation stays correct: the literal is fresh and `opts` never
//!     escapes). NO bare `opts`, `opts[expr]`, `delete opts.x`, spread, etc.
//!   * No used key collides with an identifier otherwise referenced/bound in the
//!     function (so the new param name cannot capture or be captured), and no used
//!     key is an `Object.prototype` name (see below).
//!
//! CALL SITES (every one, or we bail):
//!   * The argument at the `opts` position is an **object literal** with only plain
//!     `Init` data properties (no getter/setter — a getter would run once per read
//!     vs once as a value), no spread, no `__proto__:` setter, static identifier
//!     keys, and no duplicate keys. (A non-literal / missing arg would change
//!     `opts.x` from a value to `undefined.x`-throws, so we bail.)
//!   * A key the function never reads is dropped only if its value is
//!     side-effect-free (else we bail — its side effect must still run).
//!   * Evaluation order: a literal evaluates its values left-to-right; the split
//!     call evaluates in canonical param order. We pick one canonical order and
//!     require each site's side-effecting values to keep their relative order under
//!     it (else bail).
//!
//! MISSING KEYS read as `undefined`, which matches `opts.x` of an absent property
//! ONLY if `x` is not an inherited name. So we exclude any used key that is an
//! `Object.prototype` own-property name, and bail the whole pass if the program
//! mutates `%Object.prototype%` (which could make a missing key read an inherited
//! value instead of `undefined`).
//!
//! IDEMPOTENCE: after the rewrite the function has no `opts` param and call sites
//! pass scalars, so a re-run finds no candidate.

use std::collections::{HashMap, HashSet};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, AssignmentTarget, BindingPattern, CallExpression, Expression, FormalParameter,
    FormalParameters, Function, FunctionType, IdentifierReference, ObjectPropertyKind, Program,
    PropertyKey, PropertyKind, SimpleAssignmentTarget, StaticMemberExpression, UnaryOperator,
};
use oxc_ast::AstBuilder;
use oxc_ast_visit::Visit;
use oxc_ecmascript::constant_evaluation::ConstantEvaluationCtx;
use oxc_ecmascript::side_effects::{
    MayHaveSideEffects, MayHaveSideEffectsContext, PropertyReadSideEffects,
};
use oxc_ecmascript::GlobalContext;
use oxc_semantic::Scoping;
use oxc_span::SPAN;
use oxc_syntax::number::NumberBase;
use oxc_syntax::symbol::SymbolId;
use oxc_traverse::{Traverse, TraverseCtx};

use crate::level::OptLevel;
use crate::pass::{run_traverse, Pass, PassConfig, PassResult};
use crate::semantic_util::program_has_with_or_eval;

/// Maximum number of scalar params we will introduce (avoids signature blow-up).
const MAX_PARAMS: usize = 8;

/// `Object.prototype` own-property names: reading `opts.<name>` of an absent key
/// yields the INHERITED value, not `undefined`, so such keys are not scalarizable.
const OBJECT_PROTO_NAMES: &[&str] = &[
    "constructor",
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toLocaleString",
    "toString",
    "valueOf",
    "__proto__",
    "__defineGetter__",
    "__defineSetter__",
    "__lookupGetter__",
    "__lookupSetter__",
];

#[derive(Default)]
pub struct ParamScalarization;

impl Pass for ParamScalarization {
    fn name(&self) -> &'static str {
        "param-scalarization"
    }

    fn min_level(&self) -> OptLevel {
        OptLevel::O2
    }

    fn run<'a>(
        &mut self,
        program: &mut Program<'a>,
        scoping: &mut Scoping,
        allocator: &'a Allocator,
        cfg: &PassConfig,
    ) -> PassResult {
        if program_has_with_or_eval(program) {
            return PassResult::UNCHANGED;
        }
        // A missing key reads `undefined` only if `Object.prototype` is pristine;
        // a mutation could install an inherited value/accessor. Bail conservatively.
        if object_prototype_tainted(program, scoping) {
            return PassResult::UNCHANGED;
        }

        let ast = AstBuilder::new(allocator);

        // 1. Find candidate functions (non-escaping local fn with a scalarizable
        //    `opts` last param).
        let candidates = collect_candidates(program, scoping, cfg);
        if candidates.is_empty() {
            return PassResult::UNCHANGED;
        }

        // 2. Validate every call site of each candidate and build a rewrite plan.
        let plans = validate_and_plan(program, scoping, &candidates, ast);
        if plans.is_empty() {
            return PassResult::UNCHANGED;
        }

        // 3. Rewrite function signatures + bodies + call sites.
        let opts_symbols: HashSet<SymbolId> = plans.values().map(|p| p.opts_symbol).collect();
        let mut mutator = Mutator {
            plans,
            opts_symbols,
            changed: false,
        };
        run_traverse(&mut mutator, allocator, program, scoping, ());
        if mutator.changed {
            PassResult::CHANGED
        } else {
            PassResult::UNCHANGED
        }
    }
}

// ============================ candidate collection ============================

struct Cand {
    opts_symbol: SymbolId,
    /// Keys the function reads/writes via `opts.<key>`.
    read_keys: HashSet<String>,
    /// Index of the `opts` parameter (the last formal parameter).
    opts_index: usize,
}

/// Find every function that is a scalarization candidate, keyed by its symbol.
fn collect_candidates(
    program: &Program,
    scoping: &Scoping,
    cfg: &PassConfig,
) -> HashMap<SymbolId, Cand> {
    struct C<'s> {
        scoping: &'s Scoping,
        is_module: bool,
        exported: &'s HashSet<SymbolId>,
        out: HashMap<SymbolId, Cand>,
    }
    impl<'a, 's> Visit<'a> for C<'s> {
        fn visit_function(&mut self, func: &Function<'a>, flags: oxc_syntax::scope::ScopeFlags) {
            if let Some((sym, cand)) =
                analyze_candidate(func, self.scoping, self.is_module, self.exported)
            {
                self.out.insert(sym, cand);
            }
            oxc_ast_visit::walk::walk_function(self, func, flags);
        }
    }
    let mut c = C {
        scoping,
        is_module: cfg.is_module,
        exported: &cfg.exported,
        out: HashMap::new(),
    };
    c.visit_program(program);
    c.out
}

/// Analyze a single function for candidacy; returns its symbol + decomposition.
fn analyze_candidate(
    func: &Function,
    scoping: &Scoping,
    is_module: bool,
    exported: &HashSet<SymbolId>,
) -> Option<(SymbolId, Cand)> {
    if func.r#type != FunctionType::FunctionDeclaration {
        return None;
    }
    let fsym = func.id.as_ref()?.symbol_id.get()?;

    // Escape / observability gate: never an exported binding, never a script's
    // shared global-root function.
    if exported.contains(&fsym) {
        return None;
    }
    let at_root = scoping.symbol_scope_id(fsym) == scoping.root_scope_id();
    if at_root && !is_module {
        return None;
    }

    // Last param must be a simple identifier `opts` with no default; no rest.
    let params = &func.params;
    if params.rest.is_some() || params.items.is_empty() {
        return None;
    }
    let opts_index = params.items.len() - 1;
    let opts_param = &params.items[opts_index];
    if opts_param.initializer.is_some() {
        return None;
    }
    let BindingPattern::BindingIdentifier(opts_id) = &opts_param.pattern else {
        return None;
    };
    let opts_symbol = opts_id.symbol_id.get()?;

    let body = func.body.as_ref()?;

    // Walk the function to: collect `opts.<key>` uses, detect bad uses of `opts`,
    // gather all identifier names in scope (for collision checks), and detect
    // `arguments`.
    let mut scan = OptsScan {
        scoping,
        opts_symbol,
        keys: HashSet::new(),
        good_member_occurrences: 0,
        bad: false,
        used_names: HashSet::new(),
        uses_arguments: false,
    };
    for stmt in &body.statements {
        scan.visit_statement(stmt);
    }
    if scan.bad || scan.uses_arguments {
        return None;
    }
    // Every reference to `opts` must be the object of a good `opts.<key>` member.
    let opts_refs = scoping.get_resolved_references(opts_symbol).count();
    if opts_refs != scan.good_member_occurrences {
        return None;
    }
    let read_keys = scan.keys;
    if read_keys.is_empty() || read_keys.len() > MAX_PARAMS {
        return None;
    }
    // No used key may be an Object.prototype name (absent key would inherit it),
    // nor collide with any identifier referenced/bound in the function.
    for k in &read_keys {
        if OBJECT_PROTO_NAMES.contains(&k.as_str()) || scan.used_names.contains(k) {
            return None;
        }
    }

    Some((
        fsym,
        Cand {
            opts_symbol,
            read_keys,
            opts_index,
        },
    ))
}

/// Visitor over a function body that classifies how `opts` is used.
struct OptsScan<'s> {
    scoping: &'s Scoping,
    opts_symbol: SymbolId,
    keys: HashSet<String>,
    good_member_occurrences: usize,
    bad: bool,
    used_names: HashSet<String>,
    uses_arguments: bool,
}

impl<'s> OptsScan<'s> {
    fn is_opts(&self, expr: &Expression) -> bool {
        matches!(expr, Expression::Identifier(id) if resolve(id, self.scoping) == Some(self.opts_symbol))
    }
}

impl<'a, 's> Visit<'a> for OptsScan<'s> {
    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        // Collect names for collision detection. `arguments` is detected here too
        // (it surfaces as an unresolved identifier reference).
        if it.name == "arguments" {
            self.uses_arguments = true;
        }
        self.used_names.insert(it.name.to_string());
    }

    fn visit_binding_identifier(&mut self, it: &oxc_ast::ast::BindingIdentifier<'a>) {
        self.used_names.insert(it.name.to_string());
    }

    fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
        if self.is_opts(&it.object) {
            // `opts.<ident>` — good only if not optional (`opts?.x` short-circuits).
            if it.optional {
                self.bad = true;
            } else {
                self.good_member_occurrences += 1;
                self.keys.insert(it.property.name.to_string());
            }
            // Do NOT descend into `it.object` as a bare opts use beyond counting;
            // still walk to catch nested members in the property chain object side.
            // (The object is the opts identifier; visiting it would also bump
            // used_names with "opts", which is harmless.)
        }
        oxc_ast_visit::walk::walk_static_member_expression(self, it);
    }

    fn visit_computed_member_expression(
        &mut self,
        it: &oxc_ast::ast::ComputedMemberExpression<'a>,
    ) {
        if self.is_opts(&it.object) {
            self.bad = true; // `opts[expr]` — dynamic key, not scalarizable.
        }
        oxc_ast_visit::walk::walk_computed_member_expression(self, it);
    }

    fn visit_unary_expression(&mut self, it: &oxc_ast::ast::UnaryExpression<'a>) {
        // `delete opts.x` changes key existence; not modelable as a scalar.
        if it.operator == UnaryOperator::Delete {
            if let Expression::StaticMemberExpression(m) = &it.argument {
                if self.is_opts(&m.object) {
                    self.bad = true;
                }
            }
        }
        oxc_ast_visit::walk::walk_unary_expression(self, it);
    }
}

// ============================ call-site validation ============================

struct Plan {
    opts_symbol: SymbolId,
    opts_index: usize,
    /// Canonical scalar-parameter order (the key names, in arg order).
    param_order: Vec<String>,
}

/// One key provided at one call site (only keys the function reads).
struct SiteKey {
    key: String,
    literal_pos: usize,
    side_effect: bool,
}

/// Validate all call sites of every candidate and produce rewrite plans for the
/// ones that pass.
fn validate_and_plan<'a>(
    program: &Program<'a>,
    scoping: &Scoping,
    candidates: &HashMap<SymbolId, Cand>,
    ast: AstBuilder<'a>,
) -> HashMap<SymbolId, Plan> {
    struct V<'s, 'c, 'a> {
        scoping: &'s Scoping,
        candidates: &'c HashMap<SymbolId, Cand>,
        se: SeCtx<'a>,
        callee_counts: HashMap<SymbolId, usize>,
        invalid: HashSet<SymbolId>,
        sites: HashMap<SymbolId, Vec<Vec<SiteKey>>>,
    }
    impl<'s, 'c, 'a> Visit<'a> for V<'s, 'c, 'a> {
        fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
            if let Expression::Identifier(id) = &call.callee {
                if let Some(sym) = resolve(id, self.scoping) {
                    if let Some(cand) = self.candidates.get(&sym) {
                        *self.callee_counts.entry(sym).or_insert(0) += 1;
                        match analyze_call_site(call, cand, &self.se) {
                            Some(site_keys) => {
                                self.sites.entry(sym).or_default().push(site_keys);
                            }
                            None => {
                                self.invalid.insert(sym);
                            }
                        }
                    }
                }
            }
            oxc_ast_visit::walk::walk_call_expression(self, call);
        }
    }

    let mut v = V {
        scoping,
        candidates,
        se: SeCtx { ast },
        callee_counts: HashMap::new(),
        invalid: HashSet::new(),
        sites: HashMap::new(),
    };
    v.visit_program(program);

    let mut plans = HashMap::new();
    for (sym, cand) in candidates {
        if v.invalid.contains(sym) {
            continue;
        }
        let callee_count = v.callee_counts.get(sym).copied().unwrap_or(0);
        if callee_count == 0 {
            continue;
        }
        // Every reference to the function must be a call callee (no escape).
        if scoping.get_resolved_references(*sym).count() != callee_count {
            continue;
        }
        let sites = match v.sites.get(sym) {
            Some(s) => s,
            None => continue,
        };
        if let Some(param_order) = build_param_order(cand, sites) {
            plans.insert(
                *sym,
                Plan {
                    opts_symbol: cand.opts_symbol,
                    opts_index: cand.opts_index,
                    param_order,
                },
            );
        }
    }
    plans
}

/// Analyze one call site's `opts` argument. Returns the keys it provides that the
/// function reads (with positions + side-effect flags), or `None` if the site is
/// not scalarizable.
fn analyze_call_site(call: &CallExpression, cand: &Cand, se: &SeCtx) -> Option<Vec<SiteKey>> {
    if call.optional {
        return None; // `f?.(...)`
    }
    let args = &call.arguments;
    // No spread at/before the opts position (positions must be exact).
    for arg in args.iter().take(cand.opts_index + 1) {
        if arg.is_spread() {
            return None;
        }
    }
    // The opts argument must be present and be an object literal.
    let opts_arg = args.get(cand.opts_index)?;
    let Argument::ObjectExpression(obj) = opts_arg else {
        return None;
    };

    let mut provided: Vec<SiteKey> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (pos, prop) in obj.properties.iter().enumerate() {
        let ObjectPropertyKind::ObjectProperty(op) = prop else {
            return None; // spread
        };
        if op.kind != PropertyKind::Init || op.method || op.computed {
            return None; // getter/setter/method/computed
        }
        let key = static_key(&op.key)?; // non-identifier / non-static key
        if key == "__proto__" {
            return None; // (also blocklisted on the function side)
        }
        if !seen.insert(key.clone()) {
            return None; // duplicate key
        }
        let side_effect = op.value.may_have_side_effects(se);
        if cand.read_keys.contains(&key) {
            provided.push(SiteKey {
                key,
                literal_pos: pos,
                side_effect,
            });
        } else if side_effect {
            // Extra key the function never reads, but its value has side effects we
            // would drop. Bail rather than lose them.
            return None;
        }
        // else: extra key, side-effect-free -> dropped.
    }
    Some(provided)
}

/// Choose a canonical param order and validate that every site's side-effecting
/// values keep their relative order under it. Returns the order, or `None` if a
/// site's evaluation order cannot be preserved.
fn build_param_order(cand: &Cand, sites: &[Vec<SiteKey>]) -> Option<Vec<String>> {
    // First-appearance order across sites, then any never-provided read keys.
    let mut order: Vec<String> = Vec::new();
    let mut in_order: HashSet<String> = HashSet::new();
    for site in sites {
        for sk in site {
            if in_order.insert(sk.key.clone()) {
                order.push(sk.key.clone());
            }
        }
    }
    let mut remaining: Vec<String> = cand
        .read_keys
        .iter()
        .filter(|k| !in_order.contains(*k))
        .cloned()
        .collect();
    remaining.sort();
    order.extend(remaining);

    let pos_of: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i))
        .collect();

    // Per site: the side-effecting values, listed in canonical order, must keep
    // their literal (source) order.
    for site in sites {
        let mut effecting: Vec<(usize, usize)> = site
            .iter()
            .filter(|sk| sk.side_effect)
            .map(|sk| (pos_of[sk.key.as_str()], sk.literal_pos))
            .collect();
        effecting.sort_by_key(|(canon, _)| *canon);
        let mut prev_lit: Option<usize> = None;
        for (_, lit) in effecting {
            if let Some(p) = prev_lit {
                if lit < p {
                    return None; // reordering would swap two side effects
                }
            }
            prev_lit = Some(lit);
        }
    }
    Some(order)
}

// ================================ mutation ====================================

struct Mutator {
    plans: HashMap<SymbolId, Plan>,
    opts_symbols: HashSet<SymbolId>,
    changed: bool,
}

impl Mutator {
    fn opts_symbol_of(&self, expr: &Expression, scoping: &Scoping) -> bool {
        matches!(expr, Expression::Identifier(id)
            if resolve(id, scoping).is_some_and(|s| self.opts_symbols.contains(&s)))
    }
}

impl<'a> Traverse<'a, ()> for Mutator {
    /// Rewrite `opts.k` reads, and `opts.k = v` / `opts.k op= v` write targets, to
    /// the scalar identifier `k`. Runs post-order so member chains collapse cleanly.
    fn exit_expression(&mut self, node: &mut Expression<'a>, ctx: &mut TraverseCtx<'a, ()>) {
        let ast = ctx.ast;
        match node {
            // Read: `opts.k` -> `k`.
            Expression::StaticMemberExpression(m)
                if !m.optional && self.opts_symbol_of(&m.object, ctx.scoping()) =>
            {
                *node = ast.expression_identifier(m.property.span, m.property.name);
                self.changed = true;
            }
            // Write target: `opts.k = v` / `opts.k += v` -> `k = v` / `k += v`.
            Expression::AssignmentExpression(a) => {
                if let AssignmentTarget::StaticMemberExpression(m) = &a.left {
                    if !m.optional && self.opts_symbol_of(&m.object, ctx.scoping()) {
                        let id = ast.identifier_reference(m.property.span, m.property.name);
                        a.left = AssignmentTarget::AssignmentTargetIdentifier(ast.alloc(id));
                        self.changed = true;
                    }
                }
            }
            // Update target: `opts.k++` -> `k++`.
            Expression::UpdateExpression(u) => {
                if let SimpleAssignmentTarget::StaticMemberExpression(m) = &u.argument {
                    if !m.optional && self.opts_symbol_of(&m.object, ctx.scoping()) {
                        let id = ast.identifier_reference(m.property.span, m.property.name);
                        u.argument =
                            SimpleAssignmentTarget::AssignmentTargetIdentifier(ast.alloc(id));
                        self.changed = true;
                    }
                }
            }
            _ => {}
        }
    }

    /// Replace the `opts` parameter with the scalar params (post-order: the body's
    /// `opts.k` uses are already rewritten).
    fn exit_function(&mut self, func: &mut Function<'a>, ctx: &mut TraverseCtx<'a, ()>) {
        let Some(id) = &func.id else { return };
        let Some(sym) = id.symbol_id.get() else {
            return;
        };
        let Some(plan) = self.plans.get(&sym) else {
            return;
        };
        let ast = ctx.ast;
        rewrite_params(&mut func.params, plan, ast);
        self.changed = true;
    }

    /// Rewrite a call `f(.., {k: v, ..}, ..)` to `f(.., v_for_each_param, ..)`.
    fn exit_call_expression(
        &mut self,
        call: &mut CallExpression<'a>,
        ctx: &mut TraverseCtx<'a, ()>,
    ) {
        let Expression::Identifier(id) = &call.callee else {
            return;
        };
        let Some(sym) = resolve(id, ctx.scoping()) else {
            return;
        };
        let Some(plan) = self.plans.get(&sym) else {
            return;
        };
        let ast = ctx.ast;
        rewrite_call(call, plan, ast);
        self.changed = true;
    }
}

/// Replace the trailing `opts` param at `plan.opts_index` with one simple-identifier
/// param per key in `plan.param_order`.
fn rewrite_params<'a>(params: &mut FormalParameters<'a>, plan: &Plan, ast: AstBuilder<'a>) {
    let old = std::mem::replace(&mut params.items, ast.vec());
    let mut items = ast.vec_with_capacity(plan.opts_index + plan.param_order.len());
    for (i, p) in old.into_iter().enumerate() {
        if i != plan.opts_index {
            items.push(p);
        }
    }
    for key in &plan.param_order {
        items.push(make_param(key, ast));
    }
    params.items = items;
}

/// A simple-identifier formal parameter named `name`.
fn make_param<'a>(name: &str, ast: AstBuilder<'a>) -> FormalParameter<'a> {
    use oxc_allocator::Box as ABox;
    use oxc_ast::ast::TSTypeAnnotation;
    let owned = ast.allocator.alloc_str(name);
    let pattern = ast.binding_pattern_binding_identifier(SPAN, owned);
    ast.formal_parameter(
        SPAN,
        ast.vec(),
        pattern,
        None::<ABox<TSTypeAnnotation>>,
        None::<ABox<Expression>>,
        false,
        None,
        false,
        false,
    )
}

/// Rewrite a call's argument list, expanding the object-literal `opts` argument
/// into positional values in `plan.param_order`.
fn rewrite_call<'a>(call: &mut CallExpression<'a>, plan: &Plan, ast: AstBuilder<'a>) {
    let old = std::mem::replace(&mut call.arguments, ast.vec());

    let mut before: Vec<Argument<'a>> = Vec::new();
    let mut literal: Option<Argument<'a>> = None;
    let mut after: Vec<Argument<'a>> = Vec::new();
    for (i, arg) in old.into_iter().enumerate() {
        if i < plan.opts_index {
            before.push(arg);
        } else if i == plan.opts_index {
            literal = Some(arg);
        } else {
            after.push(arg);
        }
    }

    // Extract each provided key's value from the object literal.
    let mut provided: HashMap<String, Expression<'a>> = HashMap::new();
    if let Some(Argument::ObjectExpression(obj)) = literal {
        for prop in obj.unbox().properties {
            if let ObjectPropertyKind::ObjectProperty(op) = prop {
                let op = op.unbox();
                if let Some(key) = static_key(&op.key) {
                    provided.insert(key, op.value);
                }
            }
        }
    }

    let mut args = ast.vec_with_capacity(before.len() + plan.param_order.len() + after.len());
    for a in before {
        args.push(a);
    }
    for key in &plan.param_order {
        let value = provided.remove(key).unwrap_or_else(|| undefined_expr(ast));
        args.push(Argument::from(value));
    }
    for a in after {
        args.push(a);
    }
    call.arguments = args;
}

/// `void 0` — a side-effect-free `undefined` (safe against a shadowed `undefined`).
fn undefined_expr<'a>(ast: AstBuilder<'a>) -> Expression<'a> {
    let zero = ast.expression_numeric_literal(SPAN, 0.0, None, NumberBase::Decimal);
    ast.expression_unary(SPAN, UnaryOperator::Void, zero)
}

// ================================ helpers =====================================

fn resolve(id: &IdentifierReference, scoping: &Scoping) -> Option<SymbolId> {
    let rid = id.reference_id.get()?;
    scoping.get_reference(rid).symbol_id()
}

/// The static identifier-name form of a property key (`a` or `"a"` where `"a"` is
/// a valid identifier). Returns `None` for computed / numeric / non-identifier keys.
fn static_key(key: &PropertyKey) -> Option<String> {
    match key {
        PropertyKey::StaticIdentifier(id) => Some(id.name.to_string()),
        PropertyKey::StringLiteral(s) if is_identifier_name(s.value.as_str()) => {
            Some(s.value.to_string())
        }
        _ => None,
    }
}

/// True if `s` is a valid ECMAScript identifier name usable as a bare parameter.
fn is_identifier_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c == '$' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    s.chars()
        .skip(1)
        .all(|c| c == '_' || c == '$' || c.is_ascii_alphanumeric())
}

/// True if the program may mutate `%Object.prototype%` (assignment to
/// `Object.prototype.k`, or `Object.defineProperty`/`setPrototypeOf`/`Reflect.*`
/// targeting it). Conservative: aliasing / external taints are not detected.
fn object_prototype_tainted(program: &Program, scoping: &Scoping) -> bool {
    struct T<'s> {
        scoping: &'s Scoping,
        tainted: bool,
    }
    impl<'s> T<'s> {
        fn is_global(&self, e: &Expression, name: &str) -> bool {
            matches!(e, Expression::Identifier(id)
                if id.name == name && resolve(id, self.scoping).is_none())
        }
        fn is_object_prototype(&self, m: &StaticMemberExpression) -> bool {
            m.property.name == "prototype" && self.is_global(&m.object, "Object")
        }
        fn expr_is_object_prototype(&self, e: &Expression) -> bool {
            matches!(e, Expression::StaticMemberExpression(m) if self.is_object_prototype(m))
        }
    }
    impl<'a, 's> Visit<'a> for T<'s> {
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
        fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
            if let Expression::StaticMemberExpression(callee) = &it.callee {
                let method = callee.property.name.as_str();
                let is_mutator = (self.is_global(&callee.object, "Object")
                    && matches!(
                        method,
                        "defineProperty" | "defineProperties" | "setPrototypeOf"
                    ))
                    || (self.is_global(&callee.object, "Reflect")
                        && matches!(method, "defineProperty" | "set" | "setPrototypeOf"));
                let first_is_proto = matches!(it.arguments.first(),
                    Some(Argument::StaticMemberExpression(m)) if self.is_object_prototype(m));
                if is_mutator && first_is_proto {
                    self.tainted = true;
                }
                if matches!(method, "__defineGetter__" | "__defineSetter__")
                    && self.expr_is_object_prototype(&callee.object)
                {
                    self.tainted = true;
                }
            }
            oxc_ast_visit::walk::walk_call_expression(self, it);
        }
    }
    let mut t = T {
        scoping,
        tainted: false,
    };
    t.visit_program(program);
    t.tainted
}

/// Conservative side-effect context (no known globals; property reads and unknown
/// globals are assumed effectful), matching the other passes.
struct SeCtx<'a> {
    ast: AstBuilder<'a>,
}
impl<'a> GlobalContext<'a> for SeCtx<'a> {
    fn is_global_reference(&self, _reference: &IdentifierReference<'a>) -> bool {
        false
    }
}
impl<'a> MayHaveSideEffectsContext<'a> for SeCtx<'a> {
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
impl<'a> ConstantEvaluationCtx<'a> for SeCtx<'a> {
    fn ast(&self) -> AstBuilder<'a> {
        self.ast
    }
}
