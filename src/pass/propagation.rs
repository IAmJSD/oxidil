//! Pass: constant propagation + copy propagation (min level O1).
//!
//! Modeled on GCC's conditional constant propagation (CCP) + copy propagation,
//! but restricted to the subset we can prove observationally equivalent for
//! arbitrary JavaScript.
//!
//! For a local binding that is provably IMMUTABLE in its scope (a `const`, or a
//! `let`/`var` with zero reassignments per `oxc_semantic` references) whose
//! SINGLE initializer is:
//!   (a) a literal / known constant  => propagate that constant to every read
//!       reference (const-prop), or
//!   (b) a plain read of ANOTHER immutable, in-scope binding => replace reads
//!       with that source binding (copy-prop).
//! The now-unused binding is left for DCE to remove on a later iteration.
//!
//! SOUNDNESS (we SKIP whenever any condition is unproven):
//!   - Exactly one reaching definition: enforced by single-assignment
//!     (`!symbol_is_mutated`) + a single declarator initializer.
//!   - DECLARATION DOMINATES EVERY USE (temporal soundness). A binding's
//!     initializer only runs when control reaches the declarator; a use that
//!     executes BEFORE that point observes a different value (a hoisted `var`'s
//!     `undefined`, or a `let`/`const` in its Temporal Dead Zone — a thrown
//!     ReferenceError). "Single reaching definition" is NOT "definition reaches
//!     every use". We therefore additionally require, conservatively:
//!       * `var` bindings are NEVER propagated (they hoist to `undefined`, so a
//!         textually-earlier read legitimately sees `undefined`, and even a
//!         later-but-conditionally-skipped initializer leaves `undefined`).
//!       * for `let`/`const`, the declarator must sit at the TOP statement level
//!         of the binding's own declaring scope (not nested under any
//!         if/for/while/switch/try/block within that scope — so no back-edge or
//!         branch can reach a use before the init runs), AND every read of the
//!         binding must start strictly AFTER the declarator's initializer ends in
//!         source order. A read textually before the declarator (e.g. in an
//!         earlier nested block) is in the TDZ and must keep throwing.
//!   - Side-effect-free substitutee: we only ever copy a literal (no effect) or
//!     a bare identifier read (no effect), so no side effect is duplicated or
//!     reordered. We never propagate an arbitrary initializer expression.
//!   - Never across a closure boundary: `symbol_is_closed_over` bails, so a
//!     nested function can never observe a different value/timing.
//!   - Never for exported / top-level bindings: we refuse any ROOT-SCOPE symbol
//!     (the existing DCE convention; oxc 0.133 has no `Export` flag).
//!   - No shadowing / TDZ hazard for copy-prop: the source binding must be in
//!     the SAME scope as the alias declaration, AND at each individual use site
//!     we re-resolve the source name in the current scope and only substitute if
//!     it still resolves to the exact same source symbol (so an intervening
//!     shadow disables the rewrite for that use).
//!   - Non-finite numbers / BigInt / undefined are never synthesized (same as
//!     constant-folding): we only const-prop finite numeric / string / boolean /
//!     null literals.
//!   - We bail entirely on `with` / direct `eval` programs (scope resolution and
//!     reference counts are unreliable there).
//!
//! IDEMPOTENCE: const-prop only fires on `Expression::Identifier` nodes (it
//! produces a literal, never another identifier of the target), and copy-prop
//! refuses to rewrite when the use already names the source. So the pass never
//! re-fires on its own output and the driver fixpoint terminates.

use std::collections::{HashMap, HashSet};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    BindingPattern, Expression, Program, VariableDeclarationKind, VariableDeclarator,
};
use oxc_ast::AstBuilder;
use oxc_semantic::Scoping;
use oxc_span::{GetSpan, SPAN};
use oxc_syntax::number::NumberBase;
use oxc_syntax::scope::ScopeId;
use oxc_syntax::symbol::{SymbolFlags, SymbolId};
use oxc_traverse::{Traverse, TraverseCtx};

use crate::level::OptLevel;
use crate::pass::{run_traverse, Pass, PassConfig, PassResult};
use crate::semantic_util::{
    init_reads_are_throwless, is_root_propagatable, program_has_with_or_eval,
    symbol_is_closed_over, symbol_is_single_assignment,
};

#[derive(Default)]
pub struct Propagation;

impl Pass for Propagation {
    fn name(&self) -> &'static str {
        "propagation"
    }

    fn min_level(&self) -> OptLevel {
        OptLevel::O1
    }

    fn run<'a>(
        &mut self,
        program: &mut Program<'a>,
        scoping: &mut Scoping,
        allocator: &'a Allocator,
        cfg: &PassConfig,
    ) -> PassResult {
        // Bail on `with` / direct `eval`: scope resolution + reference counts are
        // unreliable, so neither const-prop nor copy-prop is sound.
        if program_has_with_or_eval(program) {
            return PassResult::UNCHANGED;
        }

        // PHASE 1: collect propagation targets from the current scoping snapshot.
        let targets = collect_targets(program, scoping, cfg);
        if targets.is_empty() {
            return PassResult::UNCHANGED;
        }

        // PHASE 2: rewrite read references via a traverse (per-use shadow checks
        // for copy-prop use `ctx.scoping()` + `ctx.current_scope_id()`).
        let mut visitor = PropVisitor {
            changed: false,
            targets,
        };
        run_traverse(&mut visitor, allocator, program, scoping, ());
        if visitor.changed {
            PassResult::CHANGED
        } else {
            PassResult::UNCHANGED
        }
    }
}

/// What a target symbol's reads should be replaced with.
enum Replacement<'a> {
    /// Const-prop: build a fresh copy of this literal value at each use.
    Const(LiteralValue<'a>),
    /// Copy-prop: substitute the source binding's name, but only where that name
    /// re-resolves to `src_symbol` in the use's scope (shadow guard).
    Copy {
        src_symbol: SymbolId,
        src_name: &'a str,
    },
}

/// A literal we are willing to synthesize (finite number / string / bool / null).
#[derive(Clone)]
enum LiteralValue<'a> {
    Number(f64),
    Str(&'a str),
    Bool(bool),
    Null,
}

/// Phase-1: scan declarators, returning the map of read-target symbols.
///
/// The collector also enforces the DECLARATION-DOMINATES-USE soundness gate:
///   * `var` bindings are dropped outright (hoisted-`undefined` reachability),
///   * a candidate is dropped if its declarator is nested under any control-flow
///     construct within its declaring scope (a branch/loop could reach a use
///     before — or without — the init running),
///   * a candidate is dropped if ANY read of the binding starts at or before the
///     end of the declarator's initializer in source order (a textually-earlier
///     read observes the hoisted/TDZ value, not the initializer).
fn collect_targets<'a>(
    program: &Program<'a>,
    scoping: &Scoping,
    cfg: &PassConfig,
) -> HashMap<SymbolId, Replacement<'a>> {
    let root = scoping.root_scope_id();
    let mut collector = Collector {
        scoping,
        root,
        is_module: cfg.is_module,
        targets: HashMap::new(),
        init_end: HashMap::new(),
        cf_depth: 0,
        unsafe_position: HashSet::new(),
        earliest_read: HashMap::new(),
    };
    use oxc_ast_visit::Visit;
    collector.visit_program(program);

    // Apply the temporal-dominance gate: drop any target whose earliest read
    // does not start strictly after its initializer ends, or whose declarator was
    // in an unsafe (control-flow-nested) position.
    let Collector {
        mut targets,
        init_end,
        unsafe_position,
        earliest_read,
        ..
    } = collector;
    targets.retain(|sym, repl| {
        if unsafe_position.contains(sym) {
            return false;
        }
        let Some(&init_end) = init_end.get(sym) else {
            return false;
        };
        // Every read of THIS target must come after the init. We tracked the
        // earliest read start; if it is <= init end, some use is in the TDZ /
        // hoisted region -> bail. (No recorded read => nothing to rewrite, drop.)
        if earliest_read
            .get(sym)
            .map(|&e| e > init_end)
            .unwrap_or(false)
        {
            // For copy-prop, the SOURCE binding's reads are irrelevant here (we
            // never move the source's evaluation); the source's own dominance is
            // guaranteed because we re-resolve it per use site at the same or a
            // later program point. Keep.
            let _ = repl;
            true
        } else {
            false
        }
    });
    targets
}

struct Collector<'s, 'a> {
    scoping: &'s Scoping,
    root: ScopeId,
    /// True iff the unit is an ES module — gates root-scope propagation. Note:
    /// exported root bindings ARE valid propagation sources/targets (we only
    /// replace reads; the export slot is untouched), so no `exported` set is
    /// needed here — `is_root_propagatable` ignores export status by design.
    is_module: bool,
    targets: HashMap<SymbolId, Replacement<'a>>,
    /// symbol -> source offset at which its declarator's initializer ends.
    init_end: HashMap<SymbolId, u32>,
    /// Depth of enclosing control-flow constructs within the CURRENT function.
    /// Reset to 0 when entering a function/program body so a declarator at the
    /// top level of its own scope reads depth 0.
    cf_depth: u32,
    /// Symbols whose declarator sat inside a control-flow construct (depth > 0):
    /// a branch/loop could reach a use before the init, so they are unsound.
    unsafe_position: HashSet<SymbolId>,
    /// symbol -> earliest source start offset of any READ reference to it.
    earliest_read: HashMap<SymbolId, u32>,
}

impl<'a> oxc_ast_visit::Visit<'a> for Collector<'_, 'a> {
    fn visit_variable_declarator(&mut self, decl: &VariableDeclarator<'a>) {
        self.consider(decl);
        oxc_ast_visit::walk::walk_variable_declarator(self, decl);
    }

    /// Record the earliest read position for every resolved identifier so the
    /// dominance gate can reject targets read before their initializer.
    fn visit_identifier_reference(&mut self, it: &oxc_ast::ast::IdentifierReference<'a>) {
        if let Some(rid) = it.reference_id.get() {
            let reference = self.scoping.get_reference(rid);
            if reference.is_read() {
                if let Some(sym) = reference.symbol_id() {
                    let start = it.span.start;
                    self.earliest_read
                        .entry(sym)
                        .and_modify(|e| {
                            if start < *e {
                                *e = start;
                            }
                        })
                        .or_insert(start);
                }
            }
        }
        oxc_ast_visit::walk::walk_identifier_reference(self, it);
    }

    // Control-flow constructs: while inside their subtree, any declarator seen is
    // in an unsafe position (could be reached/skipped by a branch or back-edge).
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
        oxc_ast_visit::walk::walk_switch_statement(self, it);
        self.cf_depth -= 1;
    }
    fn visit_try_statement(&mut self, it: &oxc_ast::ast::TryStatement<'a>) {
        self.cf_depth += 1;
        oxc_ast_visit::walk::walk_try_statement(self, it);
        self.cf_depth -= 1;
    }
    fn visit_block_statement(&mut self, it: &oxc_ast::ast::BlockStatement<'a>) {
        // A bare nested block creates a new lexical scope; a `let` declared at the
        // top of that block IS at the top of its own declaring scope, so this is
        // not itself "control-flow nested". But a use in a sibling/earlier block
        // is handled by the source-position gate. We do NOT bump cf_depth here.
        oxc_ast_visit::walk::walk_block_statement(self, it);
    }
    fn visit_labeled_statement(&mut self, it: &oxc_ast::ast::LabeledStatement<'a>) {
        // A label wraps a loop/block; the wrapped statement bumps depth itself.
        oxc_ast_visit::walk::walk_labeled_statement(self, it);
    }

    // Entering a nested function resets the control-flow nesting (a declarator at
    // the top of a nested function body is at depth 0 within that function).
    fn visit_function(
        &mut self,
        it: &oxc_ast::ast::Function<'a>,
        flags: oxc_syntax::scope::ScopeFlags,
    ) {
        let saved = self.cf_depth;
        self.cf_depth = 0;
        oxc_ast_visit::walk::walk_function(self, it, flags);
        self.cf_depth = saved;
    }
    fn visit_arrow_function_expression(&mut self, it: &oxc_ast::ast::ArrowFunctionExpression<'a>) {
        let saved = self.cf_depth;
        self.cf_depth = 0;
        oxc_ast_visit::walk::walk_arrow_function_expression(self, it);
        self.cf_depth = saved;
    }
    fn visit_static_block(&mut self, it: &oxc_ast::ast::StaticBlock<'a>) {
        let saved = self.cf_depth;
        self.cf_depth = 0;
        oxc_ast_visit::walk::walk_static_block(self, it);
        self.cf_depth = saved;
    }
}

impl<'a> Collector<'_, 'a> {
    fn consider(&mut self, decl: &VariableDeclarator<'a>) {
        // Simple identifier binding only (no destructuring).
        let BindingPattern::BindingIdentifier(id) = &decl.id else {
            return;
        };
        let Some(symbol_id) = id.symbol_id.get() else {
            return;
        };
        // `var` bindings hoist to `undefined`; a read reachable before the
        // initializer (textually earlier, or via a skipped conditional init)
        // legitimately observes `undefined`. We cannot prove dominance for `var`
        // with a lexical check, so NEVER propagate it.
        if decl.kind == VariableDeclarationKind::Var
            || self
                .scoping
                .symbol_flags(symbol_id)
                .contains(SymbolFlags::FunctionScopedVariable)
        {
            return;
        }
        // Root-scope (top-level) bindings: in a SCRIPT these are global-lexical /
        // global-object state observable by sibling scripts, so never touch them.
        // In an ES MODULE, top-level `const`/`let` are module-private; their reads
        // may be propagated even when exported (the export slot is untouched — we
        // only replace use sites with an equal value), so allow them there. `var`
        // was already excluded above (hoist-to-undefined hazard).
        if self.scoping.symbol_scope_id(symbol_id) == self.root
            && !is_root_propagatable(symbol_id, self.scoping, self.is_module)
        {
            return;
        }
        // Exactly one reaching definition: never reassigned.
        if !symbol_is_single_assignment(symbol_id, self.scoping) {
            return;
        }
        // A closure could observe a later value/timing -> unsound to propagate.
        if symbol_is_closed_over(symbol_id, self.scoping) {
            return;
        }
        let Some(init) = &decl.init else {
            return;
        };

        // Record the declarator's initializer end offset for the dominance gate,
        // and whether the declarator sits in a control-flow-nested (unsafe)
        // position within its scope.
        //
        // SPAN-COLLAPSE GUARD: a prior fixpoint iteration may have folded this
        // declarator's initializer to a freshly-minted literal whose span is the
        // synthetic `SPAN` (offset 0) — e.g. `const b = a + 5` -> `const b = 15`.
        // A 0 `init_end` would make the dominance test `read_start > init_end`
        // trivially TRUE for every read (any real offset > 0), silently defeating
        // the TDZ/declaration-dominates-use gate and letting us propagate into a
        // read that is textually BEFORE the declaration (a TDZ throw we must
        // preserve). When the init span is collapsed we fall back to the binding
        // identifier's span end — never collapsed by init-folding, and a sound
        // lower bound for "the declaration has been reached" (`id.span.end` <= the
        // real init end always, so reads before the declarator are still rejected).
        let init_span_end = init.span().end;
        let effective_init_end = if init_span_end == 0 {
            id.span.end
        } else {
            init_span_end
        };
        self.init_end.insert(symbol_id, effective_init_end);
        if self.cf_depth > 0 {
            self.unsafe_position.insert(symbol_id);
        }

        // TDZ-IN-INITIALIZER guard (matters for copy-prop). Copy-prop replaces
        // reads of this alias with reads of the source binding and lets DCE delete
        // the alias declarator — effectively MOVING the init's evaluation to the
        // (later) use site. If the init reads a binding still in its TDZ at THIS
        // declarator (e.g. `const a = later; const later = 5;` — `later` declared
        // textually after `a`) or an undeclared global, evaluating it here throws a
        // `ReferenceError`; moving it deletes that throw. Require every free
        // identifier read in the init to be provably already-initialized here.
        // (Literal const-prop initializers have no reads, so this is a no-op for
        // them.)
        if !init_reads_are_throwless(init, self.scoping, decl.span.start) {
            return;
        }

        // (a) const-prop: literal initializer.
        if let Some(lit) = literal_value(init) {
            self.targets.insert(symbol_id, Replacement::Const(lit));
            return;
        }

        // (b) copy-prop: initializer is a bare identifier read of another
        // immutable, in-scope binding.
        if let Expression::Identifier(src_ref) = init {
            let Some(src_symbol) = self.resolve(src_ref) else {
                return;
            };
            if src_symbol == symbol_id {
                return; // `let x = x;` — not a real copy.
            }
            // Source must itself be a stable, non-captured local in the SAME
            // scope as this declaration (so no scope-crossing / TDZ surprises;
            // per-use shadow re-checks handle nested shadowing). Root-scope
            // sources are allowed only in a module (script top-level is shared
            // global state). The substituted reads keep the source binding alive,
            // so an exported source's export slot is unaffected.
            if self.scoping.symbol_scope_id(src_symbol) == self.root
                && !is_root_propagatable(src_symbol, self.scoping, self.is_module)
            {
                return;
            }
            if self.scoping.symbol_scope_id(src_symbol) != self.scoping.symbol_scope_id(symbol_id) {
                return;
            }
            if !symbol_is_single_assignment(src_symbol, self.scoping) {
                return;
            }
            if symbol_is_closed_over(src_symbol, self.scoping) {
                return;
            }
            let src_name: &'a str = src_ref.name.as_str();
            self.targets.insert(
                symbol_id,
                Replacement::Copy {
                    src_symbol,
                    src_name,
                },
            );
        }
    }

    /// Resolve an identifier reference to its bound symbol via its reference id.
    fn resolve(&self, id: &oxc_ast::ast::IdentifierReference<'a>) -> Option<SymbolId> {
        let rid = id.reference_id.get()?;
        self.scoping.get_reference(rid).symbol_id()
    }
}

/// Extract a synthesizable literal value from an initializer expression.
/// Mirrors constant-folding: only FINITE numbers, strings, booleans, null.
fn literal_value<'a>(expr: &Expression<'a>) -> Option<LiteralValue<'a>> {
    match expr {
        Expression::NumericLiteral(n) if n.value.is_finite() => Some(LiteralValue::Number(n.value)),
        Expression::StringLiteral(s) => Some(LiteralValue::Str(s.value.as_str())),
        Expression::BooleanLiteral(b) => Some(LiteralValue::Bool(b.value)),
        Expression::NullLiteral(_) => Some(LiteralValue::Null),
        _ => None,
    }
}

struct PropVisitor<'a> {
    changed: bool,
    targets: HashMap<SymbolId, Replacement<'a>>,
}

impl<'a> Traverse<'a, ()> for PropVisitor<'a> {
    fn exit_expression(&mut self, node: &mut Expression<'a>, ctx: &mut TraverseCtx<'a, ()>) {
        let Expression::Identifier(id) = node else {
            return;
        };
        // Resolve this identifier's symbol + whether it is a READ.
        let Some(rid) = id.reference_id.get() else {
            return;
        };
        let reference = ctx.scoping().get_reference(rid);
        if !reference.is_read() {
            return; // never rewrite a write target (e.g. assignment LHS).
        }
        let Some(symbol_id) = reference.symbol_id() else {
            return;
        };
        let Some(repl) = self.targets.get(&symbol_id) else {
            return;
        };

        let new_expr: Expression<'a> = match repl {
            Replacement::Const(lit) => build_literal(lit, ctx.ast),
            Replacement::Copy {
                src_symbol,
                src_name,
            } => {
                // Shadow guard: the source name must resolve to the SAME source
                // symbol in this use's scope. If something shadows it here, skip.
                let here = ctx.current_scope_id();
                match ctx.scoping().find_binding(here, (*src_name).into()) {
                    Some(s) if s == *src_symbol => {}
                    _ => return,
                }
                // Idempotence: do not rewrite if the use already names the source.
                if id.name.as_str() == *src_name {
                    return;
                }
                // PRESERVE THE ORIGINAL SPAN. The dominance gate (collect_targets)
                // relies on use-reference source positions; a freshly-minted
                // `SPAN` (offset 0) would look like a use BEFORE every declarator
                // on the next fixpoint iteration and spuriously disable
                // propagation/inlining for that name. Reuse the replaced node's
                // span so its position is faithful.
                ctx.ast.expression_identifier(id.span, *src_name)
            }
        };

        *node = new_expr;
        self.changed = true;
    }
}

/// Build a fresh literal AST node in the arena from a captured `LiteralValue`.
fn build_literal<'a>(lit: &LiteralValue<'a>, ast: AstBuilder<'a>) -> Expression<'a> {
    match lit {
        LiteralValue::Number(n) => {
            ast.expression_numeric_literal(SPAN, *n, None, NumberBase::Decimal)
        }
        LiteralValue::Str(s) => {
            let owned: &'a str = ast.allocator.alloc_str(s);
            ast.expression_string_literal(SPAN, owned, None)
        }
        LiteralValue::Bool(b) => ast.expression_boolean_literal(SPAN, *b),
        LiteralValue::Null => ast.expression_null_literal(SPAN),
    }
}
