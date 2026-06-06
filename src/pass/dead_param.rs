//! Pass: dead/unused parameter elimination (min level O2).
//!
//! GCC-style dead argument elimination, restricted to the cases we can PROVE
//! observationally equivalent. For a LOCAL function declaration whose every use
//! is a direct call we can see, we remove TRAILING unused parameters and the
//! matching TRAILING arguments at every call site.
//!
//! The pass runs in two phases (collect, then transform) because the decision to
//! shrink a function's arity depends on whole-program facts (all call sites, all
//! references):
//!
//!   PHASE 1 (collect, immutable): build a plan mapping each eligible function
//!   symbol to a "cut index" `k` — params `[k..]` (and the matching trailing
//!   args at every call site) are removed.
//!
//!   PHASE 2 (transform, mutate via `run_traverse`): in `exit_function` truncate
//!   the planned function's params; in `exit_call_expression` truncate the
//!   matching trailing args of every call to a planned symbol.
//!
//! SOUNDNESS — a function symbol is eligible ONLY when ALL of these hold:
//!   * It is a `FunctionDeclaration` with a resolved `id` symbol (so we can find
//!     all references via the resolver).
//!   * It is NOT in the module/root scope (root-scope bindings may be exported /
//!     observed; we never touch them — the existing DCE convention).
//!   * The function is never reassigned (`!symbol_is_mutated`).
//!   * EVERY resolved reference to the symbol is the *callee* of an ordinary
//!     `CallExpression` (`foo(...)`). Any other use — passed as a value, stored,
//!     used as `new foo()`, a member callee, etc. — makes arity observable
//!     (`foo.length`, partial application) and disqualifies the function.
//!   * It is NOT a generator (arity/`arguments` interactions) — async is fine for
//!     arity but we additionally require the body to NOT mention `arguments`.
//!   * All params are simple identifiers: no rest, no default initializer, no
//!     destructuring (those make positional removal unsound or change evaluation).
//!   * It has at least one TRAILING unused param (a param whose binding symbol is
//!     unused), and we only cut a contiguous trailing run of unused params — we
//!     NEVER shift parameter positions.
//!   * At EVERY visible call site, every argument we would drop (index >= k) is
//!     side-effect-free and is not a spread element. (Dropping an impure arg, or
//!     a spread, could change behavior; per spec we simply skip such functions.)
//!
//! We bail on the whole program if it uses `with` or direct `eval` (scope
//! resolution / reference counts unreliable) — same as DCE/rename.
//!
//! IDEMPOTENCE: a function only enters the plan when it has a trailing run of
//! unused params AND at least one arg to drop or a param to drop; once params are
//! removed the corresponding bindings are gone, so the next iteration finds no
//! trailing-unused params and reports UNCHANGED.

use std::collections::{HashMap, HashSet};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, BindingPattern, CallExpression, Expression, Function, FunctionType,
    IdentifierReference, Program,
};
use oxc_ast::AstBuilder;
use oxc_ast_visit::Visit;
use oxc_ecmascript::constant_evaluation::ConstantEvaluationCtx;
use oxc_ecmascript::side_effects::{
    MayHaveSideEffects, MayHaveSideEffectsContext, PropertyReadSideEffects,
};
use oxc_ecmascript::GlobalContext;
use oxc_semantic::Scoping;
use oxc_syntax::node::NodeId;
use oxc_syntax::symbol::SymbolId;
use oxc_traverse::{Traverse, TraverseCtx};

use crate::level::OptLevel;
use crate::pass::{run_traverse, Pass, PassConfig, PassResult};
use crate::semantic_util::program_has_with_or_eval;

#[derive(Default)]
pub struct DeadParamElimination;

impl Pass for DeadParamElimination {
    fn name(&self) -> &'static str {
        "dead-param-elimination"
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
        // Whole-program bail: `with` / direct `eval` make scope resolution and
        // reference counts unreliable.
        if program_has_with_or_eval(program) {
            return PassResult::UNCHANGED;
        }

        // PHASE 1: collect the plan. Needs the AstBuilder for the side-effect Ctx.
        let plan = {
            let ast = AstBuilder::new(allocator);
            let mut collector = Collector {
                scoping,
                ast,
                candidates: HashMap::new(),
                call_callee_nodes: HashSet::new(),
                call_facts: HashMap::new(),
            };
            collector.visit_program(program);
            collector.finalize()
        };

        if plan.is_empty() {
            return PassResult::UNCHANGED;
        }

        // PHASE 2: apply the plan.
        let mut visitor = ApplyVisitor {
            changed: false,
            plan,
        };
        run_traverse(&mut visitor, allocator, program, scoping, ());
        if visitor.changed {
            PassResult::CHANGED
        } else {
            PassResult::UNCHANGED
        }
    }
}

/// Per-candidate info gathered while we still believe a function is eligible.
struct Candidate {
    /// Number of formal params.
    param_count: usize,
    /// Cut index `k`: params `[k..param_count)` are all unused (trailing run).
    cut: usize,
}

struct Collector<'a, 'b> {
    scoping: &'b Scoping,
    ast: AstBuilder<'a>,
    /// symbol -> candidate plan (only functions that LOOK eligible from their
    /// declaration; further disqualified by reference / call-site checks).
    candidates: HashMap<SymbolId, Candidate>,
    /// NodeIds of identifier references that are the callee of an ordinary call.
    call_callee_nodes: HashSet<NodeId>,
    /// symbol -> per-call-site facts (order-independent, so this is correct even
    /// when a hoisted call is visited before the function declaration).
    call_facts: HashMap<SymbolId, Vec<CallFacts>>,
}

/// Facts about ONE direct call site, recorded independent of whether the callee
/// was yet known to be a candidate (calls may be hoisted before the decl).
struct CallFacts {
    /// True if any argument is a spread element (positions become dynamic).
    has_spread: bool,
    /// Number of arguments at this call site.
    arg_count: usize,
    /// The smallest index of an impure argument, or `arg_count` if all pure.
    /// A cut `k` drops args in `[k, arg_count)`; it is safe at this call site iff
    /// `!has_spread` AND no impure arg lies in that dropped range, i.e.
    /// NOT (`k <= first_impure < arg_count`).
    first_impure: usize,
}

impl CallFacts {
    /// Is dropping trailing args from index `cut` safe at this call site?
    fn safe_for_cut(&self, cut: usize) -> bool {
        if self.has_spread {
            return false;
        }
        // Unsafe iff an impure argument sits in the dropped range [cut, arg_count).
        !(cut <= self.first_impure && self.first_impure < self.arg_count)
    }
}

impl<'a, 'b> Collector<'a, 'b> {
    /// Resolve the symbol a (possibly) callee identifier refers to.
    fn ref_symbol(&self, id: &IdentifierReference) -> Option<SymbolId> {
        let rid = id.reference_id.get()?;
        self.scoping.get_reference(rid).symbol_id()
    }

    /// After the walk, intersect every requirement and produce the final
    /// `symbol -> cut index` plan.
    fn finalize(self) -> HashMap<SymbolId, usize> {
        let mut plan = HashMap::new();
        for (sym, cand) in &self.candidates {
            // Every resolved reference must be a direct-call callee node.
            let refs = self.scoping.get_resolved_references(*sym);
            let mut all_calls = true;
            let mut any_ref = false;
            for r in refs {
                any_ref = true;
                if !self.call_callee_nodes.contains(&r.node_id()) {
                    all_calls = false;
                    break;
                }
            }
            if !all_calls {
                continue;
            }
            // Never reassigned.
            if self.scoping.symbol_is_mutated(*sym) {
                continue;
            }
            // If there are no references at all, the symbol is unused; leave the
            // whole function to DCE rather than just shrinking its arity.
            if !any_ref {
                continue;
            }
            if cand.cut >= cand.param_count {
                continue; // nothing to remove
            }
            // Validate EVERY recorded call site (order-independent): no spread,
            // and every argument we would drop (index >= cut) is pure.
            let cut = cand.cut;
            let calls_ok = self
                .call_facts
                .get(sym)
                .map(|facts| facts.iter().all(|f| f.safe_for_cut(cut)))
                .unwrap_or(true);
            if !calls_ok {
                continue;
            }
            plan.insert(*sym, cut);
        }
        plan
    }
}

impl<'a, 'b> Visit<'a> for Collector<'a, 'b> {
    fn visit_function(&mut self, func: &Function<'a>, flags: oxc_syntax::scope::ScopeFlags) {
        // Examine this function as a possible candidate BEFORE descending.
        self.consider_function(func);
        oxc_ast_visit::walk::walk_function(self, func, flags);
    }

    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        // Record direct-call callees: a bare identifier callee `foo(...)`.
        if let Expression::Identifier(id) = &call.callee {
            self.call_callee_nodes.insert(id.node_id.get());
            if let Some(sym) = self.ref_symbol(id) {
                // Record purity facts for this call site UNCONDITIONALLY (the
                // declaration may not have been visited yet due to hoisting).
                let eval = Ctx { ast: self.ast };
                let mut has_spread = false;
                let mut first_impure = call.arguments.len();
                for (i, arg) in call.arguments.iter().enumerate() {
                    match arg {
                        Argument::SpreadElement(_) => {
                            has_spread = true;
                        }
                        _ => {
                            let impure = match arg.as_expression() {
                                // An argument is "impure" (unsafe to drop) if oxc's
                                // side-effect analysis flags it, OR if it reads a
                                // lexical (let/const/class) binding that could be in
                                // its Temporal Dead Zone at the call site: such a
                                // read throws a ReferenceError, an observable side
                                // effect that `may_have_side_effects` does NOT model
                                // for a bare identifier read. Dropping the argument
                                // would delete that throw.
                                Some(e) => {
                                    e.may_have_side_effects(&eval)
                                        || expr_reads_tdz_capable_binding(e, self.scoping)
                                }
                                None => true,
                            };
                            if impure && i < first_impure {
                                first_impure = i;
                            }
                        }
                    }
                }
                self.call_facts.entry(sym).or_default().push(CallFacts {
                    has_spread,
                    arg_count: call.arguments.len(),
                    first_impure,
                });
            }
        }
        oxc_ast_visit::walk::walk_call_expression(self, call);
    }
}

impl<'a, 'b> Collector<'a, 'b> {
    fn consider_function(&mut self, func: &Function<'a>) {
        // Only ordinary function declarations with a resolved id symbol.
        if func.r#type != FunctionType::FunctionDeclaration {
            return;
        }
        let Some(id) = &func.id else { return };
        let Some(sym) = id.symbol_id.get() else {
            return;
        };
        // Never touch root-scope (possibly exported / observed) bindings.
        if self.scoping.symbol_scope_id(sym) == self.scoping.root_scope_id() {
            return;
        }
        // Generators: arity / iteration interactions — skip.
        if func.generator {
            return;
        }
        // Params must all be simple identifiers; no rest / default / destructure.
        let params = &func.params;
        if params.rest.is_some() {
            return;
        }
        let mut param_syms: Vec<SymbolId> = Vec::with_capacity(params.items.len());
        for p in &params.items {
            if p.initializer.is_some() {
                return; // default value
            }
            let BindingPattern::BindingIdentifier(pid) = &p.pattern else {
                return; // destructuring pattern
            };
            let Some(psym) = pid.symbol_id.get() else {
                return;
            };
            param_syms.push(psym);
        }
        // Body must not reference `arguments` (which would observe full arity).
        if let Some(body) = &func.body {
            let mut ad = ArgumentsDetector { found: false };
            for stmt in &body.statements {
                ad.visit_statement(stmt);
                if ad.found {
                    return;
                }
            }
        }
        // Compute the trailing run of unused params.
        let count = param_syms.len();
        let mut cut = count;
        for psym in param_syms.iter().rev() {
            if self.scoping.symbol_is_unused(*psym) {
                cut -= 1;
            } else {
                break;
            }
        }
        if cut >= count {
            return; // no trailing-unused param
        }
        self.candidates.insert(
            sym,
            Candidate {
                param_count: count,
                cut,
            },
        );
    }
}

/// True if `expr` reads (anywhere in its subtree) an identifier that resolves to
/// a lexical (`let`/`const`/`class`) binding. Such a read can be in its Temporal
/// Dead Zone at the call site and throw a ReferenceError, so a dropped argument
/// containing it is NOT safe to remove. We descend through the expression but not
/// into nested function/arrow bodies (their reads execute later, not at the call).
fn expr_reads_tdz_capable_binding(expr: &Expression, scoping: &Scoping) -> bool {
    use oxc_ast_visit::Visit;
    struct TdzScan<'s> {
        scoping: &'s Scoping,
        found: bool,
    }
    impl<'a> Visit<'a> for TdzScan<'_> {
        fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
            if self.found {
                return;
            }
            if let Some(rid) = it.reference_id.get() {
                if let Some(sym) = self.scoping.get_reference(rid).symbol_id() {
                    let flags = self.scoping.symbol_flags(sym);
                    if flags.intersects(
                        oxc_syntax::symbol::SymbolFlags::BlockScopedVariable
                            | oxc_syntax::symbol::SymbolFlags::ConstVariable
                            | oxc_syntax::symbol::SymbolFlags::Class,
                    ) {
                        self.found = true;
                    }
                }
            }
        }
        // Reads inside a nested function/arrow execute when the closure is called,
        // not at our call site, so they cannot be in the TDZ here. Do not descend.
        fn visit_function(&mut self, _f: &Function<'a>, _flags: oxc_syntax::scope::ScopeFlags) {}
        fn visit_arrow_function_expression(
            &mut self,
            _it: &oxc_ast::ast::ArrowFunctionExpression<'a>,
        ) {
        }
    }
    let mut s = TdzScan {
        scoping,
        found: false,
    };
    s.visit_expression(expr);
    s.found
}

/// Detects a use of `arguments` as a bare identifier inside a function body. We
/// do NOT descend into nested functions (they have their own `arguments`).
struct ArgumentsDetector {
    found: bool,
}

impl<'a> Visit<'a> for ArgumentsDetector {
    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        if it.name == "arguments" {
            self.found = true;
        }
    }
    // Nested functions have their own `arguments`; do not descend into them.
    fn visit_function(&mut self, _func: &Function<'a>, _flags: oxc_syntax::scope::ScopeFlags) {}
    fn visit_arrow_function_expression(&mut self, _it: &oxc_ast::ast::ArrowFunctionExpression<'a>) {
        // Arrow functions inherit `arguments` from the enclosing scope, so a use
        // of `arguments` inside an arrow DOES observe our function's arguments.
        // Descend into arrows.
        oxc_ast_visit::walk::walk_arrow_function_expression(self, _it);
    }
}

/// Phase-2 mutator: truncates params on planned functions and matching trailing
/// args on every call to a planned symbol.
struct ApplyVisitor {
    changed: bool,
    plan: HashMap<SymbolId, usize>,
}

impl<'a> Traverse<'a, ()> for ApplyVisitor {
    fn exit_function(&mut self, node: &mut Function<'a>, _ctx: &mut TraverseCtx<'a, ()>) {
        let Some(id) = &node.id else { return };
        let Some(sym) = id.symbol_id.get() else {
            return;
        };
        let Some(&cut) = self.plan.get(&sym) else {
            return;
        };
        if node.params.items.len() > cut {
            node.params.items.truncate(cut);
            self.changed = true;
        }
    }

    fn exit_call_expression(
        &mut self,
        node: &mut CallExpression<'a>,
        ctx: &mut TraverseCtx<'a, ()>,
    ) {
        let Expression::Identifier(id) = &node.callee else {
            return;
        };
        let Some(rid) = id.reference_id.get() else {
            return;
        };
        let Some(sym) = ctx.scoping().get_reference(rid).symbol_id() else {
            return;
        };
        let Some(&cut) = self.plan.get(&sym) else {
            return;
        };
        if node.arguments.len() > cut {
            node.arguments.truncate(cut);
            self.changed = true;
        }
    }
}

/// Conservative evaluation / side-effect context. Identical knobs to
/// `constant_fold.rs` / `dce.rs`: no known globals; property reads and unknown
/// globals are assumed effectful, so `may_have_side_effects` stays pessimistic.
struct Ctx<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> GlobalContext<'a> for Ctx<'a> {
    fn is_global_reference(&self, _reference: &IdentifierReference<'a>) -> bool {
        false
    }
}

impl<'a> MayHaveSideEffectsContext<'a> for Ctx<'a> {
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

impl<'a> ConstantEvaluationCtx<'a> for Ctx<'a> {
    fn ast(&self) -> AstBuilder<'a> {
        self.ast
    }
}
