//! Pass: inlining (min level O2). HIGHEST-RISK pass — favor correctness hard.
//!
//! We implement the soundest, highest-confidence slice of GCC-style inlining:
//!
//!   (a) SINGLE-USE VARIABLE INLINING. A local binding that is
//!         - a simple identifier declarator (no destructuring),
//!         - single-assignment (`const`, or a never-reassigned `let`/`var`),
//!         - NOT in the module/root scope (root may be exported/observed),
//!         - NOT closed over by a nested function (timing/identity hazard),
//!         - read EXACTLY ONCE (one resolved read reference, no writes), and
//!         - has an initializer that is provably side-effect-free,
//!       has its initializer MOVED to the single use site, and the now-empty
//!       declarator removed.
//!
//! SOUNDNESS rationale (why this is observationally equivalent):
//!   - DECLARATION DOMINATES THE USE. The single use must execute AFTER the
//!     declarator initializer runs. If the use is reachable before (a hoisted
//!     read, a TDZ read of the binding, or a read on a path that skips the
//!     init), moving the init to the use changes behavior — e.g. a `let q = 5`
//!     read by `f(q)` placed BEFORE the declaration originally throws a TDZ
//!     ReferenceError; inlining `5` deletes the throw. We require the use to be
//!     in the SAME lexical scope as the declarator (or be proven dominated) and
//!     to start strictly AFTER the declarator's initializer in source order, and
//!     the declarator must not sit under a control-flow construct that a use
//!     could bypass.
//!   - NO FREE-IDENTIFIER RE-RESOLUTION (block-shadow safety). Moving the init to
//!     the use site must not change which binding any free identifier in the init
//!     resolves to. `symbol_is_closed_over` only checks FUNCTION boundaries, so a
//!     block-scoped shadow (`{ let p = ...; use(a) }` where `a`'s init reads the
//!     outer `p`) would silently re-bind. We therefore require the use to be in
//!     the SAME scope as the declarator whenever the init reads ANY non-global
//!     identifier, so no intervening block can introduce a shadow.
//!   - PURE initializer => moving its evaluation to the use site cannot change
//!     any observable side effect ordering, and cannot drop or duplicate an
//!     effect (there are none). This is the key simplification that lets us
//!     ignore the general "preserve evaluation order / single evaluation"
//!     problem: a pure expression evaluated once at the use is identical to the
//!     same pure expression evaluated once at the declaration.
//!   - SINGLE READ => we never duplicate the expression, so even a pure-but-
//!     expensive expression is evaluated exactly once, as before.
//!   - SINGLE-ASSIGNMENT + not-closed-over => the value the use observes is the
//!     same value the declaration produced; no reassignment or closure can have
//!     changed it between declaration and use.
//!   - The property-read side-effect config is `All`, so `obj.x` initializers
//!     are treated as impure and are NOT inlined (getters may run code).
//!   - We bail on `with` / direct `eval` (scope resolution unreliable).
//!   - Root-scope / exported bindings are never touched (DCE convention; oxc
//!     0.133 has no `Export` symbol flag).
//!
//! We deliberately do NOT (yet) inline functions here: function inlining adds
//! `this`/`arguments`/recursion/capture/argument-single-evaluation hazards that
//! are far harder to prove. A conservative pass that misses that opportunity is
//! correct; an aggressive one is a bug. The single-use variable case alone
//! creates new propagation/folding opportunities for later iterations.
//!
//! IDEMPOTENCE: once a declarator's init is moved out and the declarator removed,
//! the symbol no longer exists as a target on the next scoping rebuild, so the
//! pass cannot re-fire on its own output.

use std::collections::HashMap;

use oxc_allocator::{Allocator, Vec as ArenaVec};
use oxc_ast::ast::{
    BindingPattern, Expression, IdentifierReference, Program, Statement, VariableDeclarationKind,
};
use oxc_ast::AstBuilder;
use oxc_ecmascript::constant_evaluation::ConstantEvaluationCtx;
use oxc_ecmascript::side_effects::{
    MayHaveSideEffects, MayHaveSideEffectsContext, PropertyReadSideEffects,
};
use oxc_ecmascript::GlobalContext;
use oxc_semantic::Scoping;
use oxc_span::GetSpan;
use oxc_syntax::symbol::SymbolId;
use oxc_traverse::{Traverse, TraverseCtx};

use crate::level::OptLevel;
use crate::pass::{run_traverse, Pass, PassConfig, PassResult};
use crate::semantic_util::{
    init_reads_are_throwless, is_root_optimizable, program_has_with_or_eval, symbol_is_closed_over,
    symbol_is_single_assignment,
};

#[derive(Default)]
pub struct Inlining;

impl Pass for Inlining {
    fn name(&self) -> &'static str {
        "inlining"
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
        // Bail on `with` / direct `eval`: reference counts + scope resolution are
        // unreliable, so neither "read exactly once" nor "single-assignment" can
        // be trusted.
        if program_has_with_or_eval(program) {
            return PassResult::UNCHANGED;
        }

        // STALE-SCOPING GUARD. The driver only rebuilds `Scoping` between fixpoint
        // iterations, so the `scoping` passed in reflects the tree as it was at the
        // START of this iteration — NOT the edits earlier same-iteration passes
        // (e.g. propagation rewrote `b`'s read to `a`) have made. Acting on those
        // stale reference counts can delete a still-referenced binding. Rebuild a
        // FRESH `Scoping` from the current tree so "read exactly once" /
        // single-assignment / closed-over reflect reality, and thread that fresh
        // scoping through the traversal so reference resolution matches.
        *scoping = crate::semantic_util::rebuild_scoping(program);

        // PHASE 1: which symbols are eligible single-use inline targets?
        let targets = collect_targets(program, scoping, cfg);
        if targets.is_empty() {
            return PassResult::UNCHANGED;
        }

        // PHASE 2a: extract the eligible declarators' initializers out of the
        // tree (taking ownership) and remove the now-empty declarators.
        let mut extractor = ExtractVisitor {
            changed: false,
            targets,
            extracted: HashMap::new(),
        };
        run_traverse(&mut extractor, allocator, program, scoping, ());

        if extractor.extracted.is_empty() {
            return PassResult::UNCHANGED;
        }

        // PHASE 2b: substitute each extracted init at its single use site.
        let mut subst = SubstVisitor {
            changed: false,
            extracted: extractor.extracted,
        };
        run_traverse(&mut subst, allocator, program, scoping, ());

        if extractor.changed || subst.changed {
            PassResult::CHANGED
        } else {
            PassResult::UNCHANGED
        }
    }
}

/// Phase 1: scan the program with the current scoping snapshot and return the
/// set of symbol ids that are safe single-use inline targets, after enforcing the
/// declaration-dominates-use + no-block-shadow gates.
fn collect_targets(
    program: &Program,
    scoping: &Scoping,
    cfg: &PassConfig,
) -> std::collections::HashSet<SymbolId> {
    use oxc_ast_visit::Visit;
    let root = scoping.root_scope_id();
    let mut c = Collector {
        scoping,
        root,
        is_module: cfg.is_module,
        exported: &cfg.exported,
        cf_depth: 0,
        switch_depth: 0,
        cand: HashMap::new(),
        use_span: HashMap::new(),
    };
    c.visit_program(program);

    let Collector { cand, use_span, .. } = c;
    let mut out = std::collections::HashSet::new();
    for (sym, info) in cand {
        // The single use's source position must start strictly AFTER the
        // declarator's initializer ends (declaration dominates the use in
        // straight-line order). A use textually before the declarator is a TDZ
        // read whose throw we must not delete.
        let Some(&use_start) = use_span.get(&sym) else {
            continue;
        };
        if use_start <= info.init_end {
            continue;
        }
        // The declarator must not sit under a control-flow construct that a path
        // could bypass to reach the use first.
        //
        // SAME-SCOPE BROADENING: when the single use is in the SAME lexical scope
        // as the declarator (and, per the dominance check above, textually AFTER
        // its initializer), no execution path can reach the use without first
        // executing the declarator: statements in one block run top-to-bottom, and
        // there is no way to jump into the middle of a block past an earlier
        // statement (labeled break/continue exit a block, they never skip forward
        // into it). So the declaration dominates the use regardless of any
        // enclosing `if`/loop/`switch`/`try` — `cf_nested` is only a hazard when
        // the use sits in a DIFFERENT (descendant) scope that a sibling path could
        // reach while bypassing the declarator. Keep the conservative bail for that
        // cross-scope case; relax it for the same-scope case.
        if info.cf_nested && info.use_scope != info.decl_scope {
            continue;
        }
        // SWITCH EXCEPTION to the same-scope broadening. A `switch` body is a
        // single lexical block scope, but `case` labels are ENTRY POINTS that jump
        // forward into the middle of that block, skipping earlier statements —
        // including a `const`/`let` declarator. So a declarator in one case and its
        // single read in a later case share `decl_scope == use_scope` and the read
        // is textually after the init (dominance passes), yet entering via the
        // later case reaches the read WITHOUT running the declarator's initializer:
        // a TDZ access that must throw `ReferenceError`. The "block statements run
        // top-to-bottom, can't jump into the middle" premise excludes switch case
        // labels, so keep the conservative bail whenever the declarator sits inside
        // a switch.
        if info.switch_nested {
            continue;
        }
        // Block-shadow safety: if the init reads any non-global identifier, only
        // inline when the use is in the SAME scope as the declarator, so no
        // intervening block can rebind a free identifier of the init. (A literal /
        // global-only init has no free local identifier to re-resolve, so a
        // descendant-scope use is safe for it.)
        if info.init_has_free_local && info.use_scope != info.decl_scope {
            continue;
        }
        out.insert(sym);
    }
    out
}

/// Per-candidate facts gathered during the structural walk.
struct CandInfo {
    init_end: u32,
    decl_scope: oxc_syntax::scope::ScopeId,
    use_scope: oxc_syntax::scope::ScopeId,
    cf_nested: bool,
    /// True iff the declarator sits inside a `switch` body. A switch body shares
    /// one block scope but case labels jump forward into it, so the same-scope
    /// dominance broadening is UNSOUND here (TDZ deletion across cases).
    switch_nested: bool,
    init_has_free_local: bool,
}

struct Collector<'s> {
    scoping: &'s Scoping,
    root: oxc_syntax::scope::ScopeId,
    /// True iff the unit is an ES module — gates inlining of root-scope bindings.
    is_module: bool,
    /// Top-level exported symbols — never inlined (export slot must persist).
    exported: &'s std::collections::HashSet<SymbolId>,
    /// Depth of enclosing control-flow constructs within the current function.
    cf_depth: u32,
    /// Depth of enclosing `switch` bodies within the current function. Tracked
    /// separately from `cf_depth` because the same-scope dominance broadening is
    /// never sound across switch case labels.
    switch_depth: u32,
    /// Eligible candidates and their dominance/shadow facts.
    cand: HashMap<SymbolId, CandInfo>,
    /// symbol -> source start offset of its (single) read use.
    use_span: HashMap<SymbolId, u32>,
}

impl<'a> oxc_ast_visit::Visit<'a> for Collector<'_> {
    fn visit_variable_declarator(&mut self, decl: &oxc_ast::ast::VariableDeclarator<'a>) {
        self.consider(decl);
        oxc_ast_visit::walk::walk_variable_declarator(self, decl);
    }

    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        if let Some(rid) = it.reference_id.get() {
            let reference = self.scoping.get_reference(rid);
            if reference.is_read() {
                if let Some(sym) = reference.symbol_id() {
                    // A candidate is read exactly once, so this is the use.
                    self.use_span.entry(sym).or_insert(it.span.start);
                }
            }
        }
        oxc_ast_visit::walk::walk_identifier_reference(self, it);
    }

    fn visit_if_statement(&mut self, it: &oxc_ast::ast::IfStatement<'a>) {
        self.cf_depth += 1;
        oxc_ast_visit::walk::walk_if_statement(self, it);
        self.cf_depth -= 1;
    }
    fn visit_for_statement(&mut self, it: &oxc_ast::ast::ForStatement<'a>) {
        self.cf_depth += 1;
        oxc_ast_visit::walk::walk_for_statement(self, it);
        self.cf_depth -= 1;
    }
    fn visit_for_in_statement(&mut self, it: &oxc_ast::ast::ForInStatement<'a>) {
        self.cf_depth += 1;
        oxc_ast_visit::walk::walk_for_in_statement(self, it);
        self.cf_depth -= 1;
    }
    fn visit_for_of_statement(&mut self, it: &oxc_ast::ast::ForOfStatement<'a>) {
        self.cf_depth += 1;
        oxc_ast_visit::walk::walk_for_of_statement(self, it);
        self.cf_depth -= 1;
    }
    fn visit_while_statement(&mut self, it: &oxc_ast::ast::WhileStatement<'a>) {
        self.cf_depth += 1;
        oxc_ast_visit::walk::walk_while_statement(self, it);
        self.cf_depth -= 1;
    }
    fn visit_do_while_statement(&mut self, it: &oxc_ast::ast::DoWhileStatement<'a>) {
        self.cf_depth += 1;
        oxc_ast_visit::walk::walk_do_while_statement(self, it);
        self.cf_depth -= 1;
    }
    fn visit_switch_statement(&mut self, it: &oxc_ast::ast::SwitchStatement<'a>) {
        self.cf_depth += 1;
        self.switch_depth += 1;
        oxc_ast_visit::walk::walk_switch_statement(self, it);
        self.switch_depth -= 1;
        self.cf_depth -= 1;
    }
    fn visit_try_statement(&mut self, it: &oxc_ast::ast::TryStatement<'a>) {
        self.cf_depth += 1;
        oxc_ast_visit::walk::walk_try_statement(self, it);
        self.cf_depth -= 1;
    }
    fn visit_function(
        &mut self,
        it: &oxc_ast::ast::Function<'a>,
        flags: oxc_syntax::scope::ScopeFlags,
    ) {
        let saved = self.cf_depth;
        let saved_sw = self.switch_depth;
        self.cf_depth = 0;
        self.switch_depth = 0;
        oxc_ast_visit::walk::walk_function(self, it, flags);
        self.cf_depth = saved;
        self.switch_depth = saved_sw;
    }
    fn visit_arrow_function_expression(&mut self, it: &oxc_ast::ast::ArrowFunctionExpression<'a>) {
        let saved = self.cf_depth;
        let saved_sw = self.switch_depth;
        self.cf_depth = 0;
        self.switch_depth = 0;
        oxc_ast_visit::walk::walk_arrow_function_expression(self, it);
        self.cf_depth = saved;
        self.switch_depth = saved_sw;
    }
    fn visit_static_block(&mut self, it: &oxc_ast::ast::StaticBlock<'a>) {
        let saved = self.cf_depth;
        let saved_sw = self.switch_depth;
        self.cf_depth = 0;
        self.switch_depth = 0;
        oxc_ast_visit::walk::walk_static_block(self, it);
        self.cf_depth = saved;
        self.switch_depth = saved_sw;
    }
}

impl Collector<'_> {
    fn consider(&mut self, decl: &oxc_ast::ast::VariableDeclarator) {
        // Simple identifier binding only.
        let BindingPattern::BindingIdentifier(id) = &decl.id else {
            return;
        };
        let Some(symbol_id) = id.symbol_id.get() else {
            return;
        };
        // Root-scope (top-level) bindings: in a SCRIPT these are observable
        // global-lexical/global-object state; never inline them. In an ES MODULE,
        // a NON-EXPORTED top-level `const`/`let` is module-private and may be
        // inlined; an EXPORTED one must keep its binding (the export slot is a
        // live binding observed by importers), so it is excluded here. The
        // extractor below only moves `const`/`let`, so top-level `var` is never
        // affected regardless.
        if self.scoping.symbol_scope_id(symbol_id) == self.root
            && !is_root_optimizable(symbol_id, self.scoping, self.is_module, self.exported)
        {
            return;
        }
        // Must have an initializer to inline.
        let Some(init) = &decl.init else {
            return;
        };
        // Single-assignment: never reassigned.
        if !symbol_is_single_assignment(symbol_id, self.scoping) {
            return;
        }
        // Not captured by a closure (timing/identity hazard).
        if symbol_is_closed_over(symbol_id, self.scoping) {
            return;
        }
        // Read EXACTLY ONCE, and that single reference must be a pure read (not a
        // write, not read-write). Writes are excluded by single-assignment already
        // (the declarator's binding is not a "write" reference), so the resolved
        // references here are pure reads.
        let refs = self.scoping.get_resolved_reference_ids(symbol_id);
        if refs.len() != 1 {
            return;
        }
        // The single reference must be a read (defensive; a never-mutated symbol's
        // references are reads).
        let only = self.scoping.get_reference(refs[0]);
        if !only.is_read() || only.is_write() {
            return;
        }

        // Record dominance / shadow facts for the post-walk gate.
        //
        // SPAN-COLLAPSE GUARD: a prior fixpoint iteration may have folded this
        // declarator's initializer to a freshly-minted literal carrying the
        // synthetic `SPAN` (offset 0) — e.g. `const b = a + 5` -> `const b = 15`.
        // A 0 `init_end` makes the dominance test `use_start > init_end` trivially
        // TRUE for every use, silently defeating the declaration-dominates-use /
        // TDZ gate and letting us inline into a use textually BEFORE the
        // declaration (a TDZ throw we must preserve). When the init span is
        // collapsed, fall back to the binding identifier's span end (never
        // collapsed by init-folding; a sound lower bound for the declaration
        // point, so a use before the declarator is still rejected).
        // TDZ-IN-INITIALIZER guard. Inlining moves this init to the (later) use
        // site and deletes the declarator. If the init reads a sibling lexical
        // binding that is in its Temporal Dead Zone at THIS declarator (declared
        // textually later) or an undeclared global, evaluating it here throws a
        // `ReferenceError`; moving it past the binding's init (or to a later point)
        // would delete that throw. Only inline when every free identifier read in
        // the init is provably already-initialized at the declarator. `var`/
        // function reads are hoist-initialized and always allowed.
        if !init_reads_are_throwless(init, self.scoping, decl.span.start) {
            return;
        }

        let init_span_end = init.span().end;
        let effective_init_end = if init_span_end == 0 {
            id.span.end
        } else {
            init_span_end
        };
        self.cand.insert(
            symbol_id,
            CandInfo {
                init_end: effective_init_end,
                decl_scope: self.scoping.symbol_scope_id(symbol_id),
                use_scope: only.scope_id(),
                cf_nested: self.cf_depth > 0,
                switch_nested: self.switch_depth > 0,
                init_has_free_local: init_has_free_local_ident(init, self.scoping),
            },
        );

        // Initializer purity is verified in the extractor phase (which has an
        // arena-backed `AstBuilder`). Here we only record structural / scoping
        // eligibility; the extractor re-checks `may_have_side_effects` before
        // committing, so a non-pure init is simply never extracted.
    }
}

/// True if `init` reads at least one identifier that resolves to a LOCAL symbol
/// (a binding whose meaning could change if the init is moved into a scope that
/// shadows it). Global/unresolved reads are excluded (no local binding to
/// re-resolve; an `init_reads_mutated_binding` check already rejects those).
fn init_has_free_local_ident(init: &Expression, scoping: &Scoping) -> bool {
    use oxc_ast_visit::Visit;
    struct Scan<'s> {
        scoping: &'s Scoping,
        found: bool,
    }
    impl<'a> Visit<'a> for Scan<'_> {
        fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
            if self.found {
                return;
            }
            if let Some(rid) = it.reference_id.get() {
                if self.scoping.get_reference(rid).symbol_id().is_some() {
                    self.found = true;
                }
            }
        }
    }
    let mut s = Scan {
        scoping,
        found: false,
    };
    s.visit_expression(init);
    s.found
}

/// Phase 2a: take eligible initializers out of the tree and drop their (now
/// empty) declarators. Re-verifies initializer purity here (where an arena-backed
/// `AstBuilder` is available) before committing.
struct ExtractVisitor<'a> {
    changed: bool,
    targets: std::collections::HashSet<SymbolId>,
    extracted: HashMap<SymbolId, Expression<'a>>,
}

impl<'a> Traverse<'a, ()> for ExtractVisitor<'a> {
    fn exit_statements(
        &mut self,
        node: &mut ArenaVec<'a, Statement<'a>>,
        ctx: &mut TraverseCtx<'a, ()>,
    ) {
        let ast = ctx.ast;
        let eval = InlineCtx { ast };

        for stmt in node.iter_mut() {
            let Statement::VariableDeclaration(var_decl) = stmt else {
                continue;
            };
            // Only `const`/`let` (var hoisting/TDZ-free redeclare semantics make
            // `var` riskier; keep conservative and skip it).
            if !matches!(
                var_decl.kind,
                VariableDeclarationKind::Const | VariableDeclarationKind::Let
            ) {
                continue;
            }
            for decl in var_decl.declarations.iter_mut() {
                let BindingPattern::BindingIdentifier(id) = &decl.id else {
                    continue;
                };
                let Some(symbol_id) = id.symbol_id.get() else {
                    continue;
                };
                if !self.targets.contains(&symbol_id) {
                    continue;
                }
                if self.extracted.contains_key(&symbol_id) {
                    continue;
                }
                let Some(init) = &decl.init else {
                    continue;
                };
                // Re-verify purity now that we have an arena-backed AstBuilder.
                if init.may_have_side_effects(&eval) {
                    continue;
                }
                // REASSIGNMENT HAZARD: the init is moved from the declaration
                // point to the (later) use site. If the init reads any binding
                // that is reassigned anywhere, the moved evaluation could observe
                // a DIFFERENT value than it would at the declaration point (e.g.
                // `let b = a; a = 5; use(b)` must keep b's original `a`). Require
                // every free identifier the init reads to be single-assignment
                // (never mutated), so its value is identical at any program point.
                if init_reads_mutated_binding(init, ctx.scoping()) {
                    continue;
                }
                // CHAIN HAZARD: if this init reads ANOTHER symbol that is itself
                // an inline target, the two extractions can collide (the other
                // target's declarator may be removed, or its use — which is this
                // init — moved). Refuse to extract chained targets in one pass;
                // the cascade resolves safely across fixpoint iterations (scoping
                // is rebuilt between them).
                if init_reads_other_target(init, ctx.scoping(), &self.targets, symbol_id) {
                    continue;
                }
                // Commit: move the init out.
                let taken = decl.init.take().expect("init present");
                self.extracted.insert(symbol_id, taken);
                self.changed = true;
            }
            // Drop declarators whose init we extracted (they bind a now-unused
            // symbol that will be substituted away at the single use site).
            var_decl.declarations.retain(|d| match &d.id {
                BindingPattern::BindingIdentifier(id) => id
                    .symbol_id
                    .get()
                    .map(|s| !self.extracted.contains_key(&s))
                    .unwrap_or(true),
                _ => true,
            });
        }

        // Remove any VariableDeclaration left with zero declarators.
        node.retain(|stmt| match stmt {
            Statement::VariableDeclaration(v) => !v.declarations.is_empty(),
            _ => true,
        });
    }
}

/// True if `init` reads any identifier binding that is reassigned/mutated
/// somewhere (or an unresolved/global reference, treated conservatively as
/// possibly-mutable). Moving such an init to a later use site could observe a
/// different value, so we refuse to inline it.
fn init_reads_mutated_binding(init: &Expression, scoping: &Scoping) -> bool {
    use oxc_ast_visit::Visit;
    struct MutScan<'s> {
        scoping: &'s Scoping,
        hazard: bool,
    }
    impl<'a> Visit<'a> for MutScan<'_> {
        fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
            if self.hazard {
                return;
            }
            match it.reference_id.get() {
                Some(rid) => match self.scoping.get_reference(rid).symbol_id() {
                    // Resolved local: hazard iff it can be reassigned.
                    Some(sym) => {
                        if self.scoping.symbol_is_mutated(sym) {
                            self.hazard = true;
                        }
                    }
                    // Resolved reference with no symbol => unresolved/global; a
                    // global binding could change between decl and use. Bail.
                    None => self.hazard = true,
                },
                // No reference id at all: be conservative.
                None => self.hazard = true,
            }
        }
    }
    let mut s = MutScan {
        scoping,
        hazard: false,
    };
    s.visit_expression(init);
    s.hazard
}

/// True if `init` reads any symbol (other than `self_sym`) that is also an
/// inline target this pass — meaning extracting both could collide. We then skip
/// the chained one and let the next fixpoint iteration handle it.
fn init_reads_other_target(
    init: &Expression,
    scoping: &Scoping,
    targets: &std::collections::HashSet<SymbolId>,
    self_sym: SymbolId,
) -> bool {
    use oxc_ast_visit::Visit;
    struct Scan<'s> {
        scoping: &'s Scoping,
        targets: &'s std::collections::HashSet<SymbolId>,
        self_sym: SymbolId,
        hit: bool,
    }
    impl<'a> Visit<'a> for Scan<'_> {
        fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
            if self.hit {
                return;
            }
            if let Some(rid) = it.reference_id.get() {
                if let Some(sym) = self.scoping.get_reference(rid).symbol_id() {
                    if sym != self.self_sym && self.targets.contains(&sym) {
                        self.hit = true;
                    }
                }
            }
        }
    }
    let mut s = Scan {
        scoping,
        targets,
        self_sym,
        hit: false,
    };
    s.visit_expression(init);
    s.hit
}

/// Phase 2b: replace the single use of each extracted symbol with its init.
struct SubstVisitor<'a> {
    changed: bool,
    extracted: HashMap<SymbolId, Expression<'a>>,
}

impl<'a> Traverse<'a, ()> for SubstVisitor<'a> {
    fn exit_expression(&mut self, node: &mut Expression<'a>, ctx: &mut TraverseCtx<'a, ()>) {
        let Expression::Identifier(id) = node else {
            return;
        };
        let Some(rid) = id.reference_id.get() else {
            return;
        };
        let reference = ctx.scoping().get_reference(rid);
        let Some(symbol_id) = reference.symbol_id() else {
            return;
        };
        // Take the extracted init out of the map (so it is used exactly once).
        let Some(init) = self.extracted.remove(&symbol_id) else {
            return;
        };
        *node = init;
        self.changed = true;
    }
}

/// Evaluation / side-effect context: same conservative knobs as the folding and
/// DCE passes. Property reads and unknown globals are assumed effectful, so e.g.
/// `obj.x` initializers are NOT considered pure and won't be inlined.
struct InlineCtx<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> GlobalContext<'a> for InlineCtx<'a> {
    fn is_global_reference(&self, _reference: &IdentifierReference<'a>) -> bool {
        false
    }
}

impl<'a> MayHaveSideEffectsContext<'a> for InlineCtx<'a> {
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

impl<'a> ConstantEvaluationCtx<'a> for InlineCtx<'a> {
    fn ast(&self) -> AstBuilder<'a> {
        self.ast
    }
}
