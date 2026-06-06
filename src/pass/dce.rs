//! Pass #3: dead-code elimination (min level O2).
//!
//! Removes provably-dead code using a mix of structural reasoning and
//! `oxc_semantic` `Scoping` reference counts. Implemented as a `Traverse`
//! visitor in the same shape as the reference `constant_fold.rs`: a struct with
//! a `changed` flag, mutation in `exit_*` (post-order, so children/branches are
//! already simplified by earlier passes and by inner DCE), nodes built via
//! `ctx.ast` (the SAME arena), and bridged through `run_traverse`.
//!
//! TRANSFORMS (each gated by a soundness condition; when it cannot be proven we
//! KEEP the code):
//!
//!   T1  Empty statements (`;`) are dropped from any statement list. Always safe.
//!
//!   T2  Unreachable code after an UNCONDITIONAL terminator (`return` / `throw` /
//!       `break` / `continue`) in the SAME statement list is truncated. Because
//!       `var` and `function` declarations HOIST out of the dead tail, we refuse
//!       the truncation if the dropped tail contains any hoisted binding
//!       (`var`, `function`, or `class` declaration, even nested inside blocks).
//!       This is conservative but sound.
//!
//!   T3  `if (<constant>) C else A` collapses to the taken branch, relying on
//!       constant-folding having run first so the test is a literal. We only fire
//!       when the test is a side-effect-free literal AND the DEAD branch contains
//!       no hoisted bindings (var/function/class), so hoisting semantics are
//!       preserved. The taken branch is wrapped in a `{ ... }` block to preserve
//!       its lexical scope. `if (false) C` with no else becomes `;` (then dropped
//!       by T1 on the next iteration).
//!
//!   T4  Unused LOCAL `let`/`const` bindings and unused LOCAL `function`
//!       declarations are removed when:
//!         - the binding is a simple identifier (no destructuring),
//!         - `scoping.symbol_is_unused(symbol)` is true (zero resolved refs),
//!         - the binding is NOT in the module/root scope (never touch top-level,
//!           which may be exported or observed), and
//!         - for `let`/`const`, the initializer is side-effect-free (so dropping
//!           it changes nothing observable). `var` is never removed (hoisting /
//!           TDZ-free redeclare semantics). Function declarations have no
//!           initializer side effects, so unused ones are removed directly.
//!       Exports, getters/setters, and anything referenced keep their bindings:
//!       an exported symbol is not reported unused by the resolver. We ALSO bail
//!       on any program that contains `with` OR a direct `eval(...)` call (see
//!       `run`). This is required for soundness: `symbol_is_unused` counts only
//!       syntactic references, so a binding referenced only from inside a string
//!       passed to direct `eval` (e.g. `eval("secret")`) would otherwise be
//!       reported unused and wrongly deleted, producing a runtime ReferenceError.
//!
//! SOUNDNESS NOTE on reference counts: the driver rebuilds `Scoping` fresh at the
//! start of every fixpoint iteration at O2+, so after we delete a binding the
//! counts are recomputed before T4 runs again — a binding only referenced by
//! other now-deleted dead code becomes removable on a later iteration.

use oxc_allocator::{Allocator, Vec as ArenaVec};
use oxc_ast::ast::{
    BindingPattern, Expression, ForStatementInit, IdentifierReference, Program, Statement,
    VariableDeclarationKind, VariableDeclarator,
};
use oxc_ast::AstBuilder;
use oxc_ecmascript::constant_evaluation::{ConstantEvaluation, ConstantEvaluationCtx};
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
use crate::semantic_util::{init_reads_are_throwless, program_has_with_or_eval};

#[derive(Default)]
pub struct DeadCodeElimination;

impl Pass for DeadCodeElimination {
    fn name(&self) -> &'static str {
        "dead-code-elimination"
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
        // Conservative whole-program bail-out: if the source uses `with`, scope
        // resolution can be defeated and reference counts are unreliable, so we
        // skip the symbol-based removal entirely. Structural transforms (T1-T3)
        // are still sound under `with`, but to keep the pass simple and safe we
        // disable ALL of DCE when `with` is present.
        // We also bail on direct `eval`: `symbol_is_unused` counts only syntactic
        // references, so a binding referenced solely from inside an `eval("...")`
        // string would be wrongly reported unused and deleted (runtime
        // ReferenceError). Treat direct eval exactly like `with`.
        let has_with_or_eval = program_has_with_or_eval(program);

        let mut visitor = DceVisitor {
            changed: false,
            allow_symbol_removal: !has_with_or_eval,
            is_module: cfg.is_module,
            exported: cfg.exported.clone(),
        };
        run_traverse(&mut visitor, allocator, program, scoping, ());
        if visitor.changed {
            PassResult::CHANGED
        } else {
            PassResult::UNCHANGED
        }
    }
}

struct DceVisitor {
    changed: bool,
    /// Disabled (false) when the program contains `with`, which makes scope
    /// resolution unreliable.
    allow_symbol_removal: bool,
    /// True iff the unit is an ES module — gates T4 removal of root-scope
    /// (top-level) `const`/`let`/`function` bindings.
    is_module: bool,
    /// Top-level exported symbols — never removed (export slot must persist).
    exported: std::rc::Rc<std::collections::HashSet<oxc_syntax::symbol::SymbolId>>,
}

impl<'a> Traverse<'a, ()> for DceVisitor {
    /// All statement-list transforms run here, post-order, so inner blocks are
    /// already simplified and any nested DCE has happened. `node` is the arena
    /// `Vec<Statement>` of a program / block / function body / switch case, and
    /// we rebuild it in place.
    fn exit_statements(
        &mut self,
        node: &mut ArenaVec<'a, Statement<'a>>,
        ctx: &mut TraverseCtx<'a, ()>,
    ) {
        let scope_id = ctx.current_scope_id();
        let is_root = scope_id == ctx.scoping().root_scope_id();
        // Root-scope removal is allowed only in an ES module (script top-level is
        // global-lexical/global-object state, observable by sibling scripts). In a
        // module, top-level bindings are module-private; the per-symbol `exported`
        // check below still protects export slots.
        let root_removal_ok = !is_root || self.is_module;

        // --- T3: expand `if(constant)` before anything else, so the chosen
        // branch participates in T1/T2 in this same pass. ---
        self.expand_constant_ifs(node, ctx);

        // --- T2: truncate unreachable tail after an unconditional terminator. ---
        self.truncate_after_terminator(node);

        // --- T1 + T4: filter out empty statements and unused local bindings. ---
        self.filter_statements(node, ctx, root_removal_ok);
    }
}

impl DceVisitor {
    /// T3: replace each `if (<const>) C else A` with its taken branch (wrapped in
    /// a block) when the test is a side-effect-free literal and the dead branch
    /// hoists nothing. Mutates statements in place.
    fn expand_constant_ifs<'a>(
        &mut self,
        node: &mut ArenaVec<'a, Statement<'a>>,
        ctx: &mut TraverseCtx<'a, ()>,
    ) {
        for i in 0..node.len() {
            let ast = ctx.ast;
            let eval = DceCtx { ast };
            let Statement::IfStatement(if_stmt) = &mut node[i] else {
                continue;
            };
            // Test must be side-effect-free with a statically-known truthiness.
            if if_stmt.test.may_have_side_effects(&eval) {
                continue;
            }
            let Some(truthy) = if_stmt.test.evaluate_value_to_boolean(&eval) else {
                continue;
            };

            // Identify taken vs dead branch.
            let dead_branch: Option<&Statement> = if truthy {
                if_stmt.alternate.as_ref()
            } else {
                Some(&if_stmt.consequent)
            };
            // Refuse if the dead branch hoists anything.
            if let Some(dead) = dead_branch {
                if stmt_has_hoisted_binding(dead) {
                    continue;
                }
            }

            // Move the taken branch out, wrap in a block to keep its scope.
            let taken: Statement<'a> = if truthy {
                std::mem::replace(&mut if_stmt.consequent, ast.statement_empty(SPAN))
            } else {
                match if_stmt.alternate.take() {
                    Some(a) => a,
                    None => ast.statement_empty(SPAN),
                }
            };
            // Give the synthesized block a fresh child scope id. Without it the
            // block's `scope_id` stays `None`, and any later TRAVERSE-based pass
            // (e.g. licm, registered after DCE) panics: the traverse walker
            // `.unwrap()`s a block statement's scope id on entry. A subsequent
            // semantic rebuild reassigns scopes correctly regardless.
            let scope_id = ctx.create_child_scope_of_current(ScopeFlags::empty());
            let ast = ctx.ast;
            let mut body = ast.vec_with_capacity(1);
            body.push(taken);
            node[i] = ast.statement_block_with_scope_id(SPAN, body, scope_id);
            self.changed = true;
        }
    }

    /// T2: if a statement in the list is an unconditional terminator, drop every
    /// statement after it — but only when that dropped tail hoists nothing.
    fn truncate_after_terminator<'a>(&mut self, node: &mut ArenaVec<'a, Statement<'a>>) {
        let Some(term_idx) = node.iter().position(is_unconditional_terminator) else {
            return;
        };
        if term_idx + 1 >= node.len() {
            return; // nothing after the terminator
        }
        // The tail [term_idx+1 ..] is unreachable. Keep it if it hoists anything.
        if node[term_idx + 1..].iter().any(stmt_has_hoisted_binding) {
            return;
        }
        node.truncate(term_idx + 1);
        self.changed = true;
    }

    /// T1 + T4: rebuild the list, dropping empty statements and unused local
    /// `let`/`const`/`function` bindings.
    fn filter_statements<'a>(
        &mut self,
        node: &mut ArenaVec<'a, Statement<'a>>,
        ctx: &mut TraverseCtx<'a, ()>,
        root_removal_ok: bool,
    ) {
        let ast = ctx.ast;
        // Decide removability up front (immutable scoping borrow), then rebuild.
        let mut remove = vec![false; node.len()];
        let mut any = false;
        for (i, stmt) in node.iter().enumerate() {
            let drop = match stmt {
                // T1: empty statement.
                Statement::EmptyStatement(_) => true,
                // T4: unused local function declaration (no init side effects).
                Statement::FunctionDeclaration(func)
                    if self.allow_symbol_removal && root_removal_ok =>
                {
                    func.id
                        .as_ref()
                        .and_then(|id| id.symbol_id.get())
                        .map(|sym| {
                            ctx.scoping().symbol_is_unused(sym) && !self.exported.contains(&sym)
                        })
                        .unwrap_or(false)
                }
                _ => false,
            };
            if drop {
                remove[i] = true;
                any = true;
            }
        }

        // T4 for variable declarations is per-declarator: handled by mutating the
        // VariableDeclaration's `declarations` vec in place (a decl may keep some
        // declarators and drop others).
        if self.allow_symbol_removal && root_removal_ok {
            for stmt in node.iter_mut() {
                if let Statement::VariableDeclaration(var_decl) = stmt {
                    if matches!(
                        var_decl.kind,
                        VariableDeclarationKind::Let | VariableDeclarationKind::Const
                    ) {
                        let eval = DceCtx { ast };
                        let before = var_decl.declarations.len();
                        var_decl.declarations.retain(|d| {
                            !is_removable_declarator(d, ctx.scoping(), &eval, &self.exported)
                        });
                        if var_decl.declarations.len() != before {
                            self.changed = true;
                        }
                    }
                }
            }
            // A VariableDeclaration whose declarators all got removed is now empty
            // and must be dropped from the statement list.
            for (i, stmt) in node.iter().enumerate() {
                if let Statement::VariableDeclaration(var_decl) = stmt {
                    if var_decl.declarations.is_empty() {
                        remove[i] = true;
                        any = true;
                    }
                }
            }
        }

        if !any {
            return;
        }

        let drained: std::vec::Vec<Statement<'a>> = node.drain(..).collect();
        let mut rebuilt = ast.vec_with_capacity(drained.len());
        for (i, stmt) in drained.into_iter().enumerate() {
            if remove[i] {
                self.changed = true;
            } else {
                rebuilt.push(stmt);
            }
        }
        *node = rebuilt;
    }
}

/// T4 helper: a `let`/`const` declarator is removable when it binds a simple,
/// unused identifier whose initializer is side-effect-free (or absent).
fn is_removable_declarator<'a>(
    d: &VariableDeclarator<'a>,
    scoping: &Scoping,
    eval: &DceCtx<'a>,
    exported: &std::collections::HashSet<oxc_syntax::symbol::SymbolId>,
) -> bool {
    // Only simple identifier bindings (no destructuring patterns).
    let BindingPattern::BindingIdentifier(id) = &d.id else {
        return false;
    };
    let Some(symbol_id) = id.symbol_id.get() else {
        return false;
    };
    // Exported bindings have a live export slot observed by importers — keep them
    // even when they appear unused within this module.
    if exported.contains(&symbol_id) {
        return false;
    }
    if !scoping.symbol_is_unused(symbol_id) {
        return false;
    }
    // Initializer must be provably free of observable side effects AND provably
    // THROWLESS w.r.t. identifier reads. `may_have_side_effects` treats a bare
    // identifier read as effect-free, but deleting `const unused = later;` where
    // `later` is a sibling lexical binding still in its TDZ (declared later), or
    // an undeclared global, would delete an observable `ReferenceError` throw.
    // `init_reads_are_throwless` rejects any free identifier read that is not
    // provably already-initialized at this declarator's program point.
    match &d.init {
        None => true,
        Some(init) => {
            !init.may_have_side_effects(eval)
                && init_reads_are_throwless(init, scoping, d.span.start)
        }
    }
}

/// True if `stmt` is an unconditional control-flow terminator: any statement
/// after it in the same list cannot execute.
///
/// A bare `{ ... }` block is a terminator iff its body cannot complete normally,
/// i.e. its last statement is itself an unconditional terminator. This is sound:
/// a block completes normally only if its last reachable statement does, and a
/// non-labeled block cannot be `break`-ed out of. (We require the block to be
/// non-empty; an empty block completes normally.) This lets T2 see through the
/// blocks that T3 introduces when collapsing `if (true) { return x; }`.
fn is_unconditional_terminator(stmt: &Statement) -> bool {
    match stmt {
        Statement::ReturnStatement(_)
        | Statement::ThrowStatement(_)
        | Statement::BreakStatement(_)
        | Statement::ContinueStatement(_) => true,
        Statement::BlockStatement(b) => b
            .body
            .last()
            .map(is_unconditional_terminator)
            .unwrap_or(false),
        _ => false,
    }
}

/// Conservatively reports whether a statement (or anything nested inside plain
/// blocks / labels) introduces a HOISTED binding (`var`, `function`, or `class`
/// declaration). Used to refuse T2/T3 removals that would change hoisting.
///
/// We descend only through constructs that do NOT create a new function scope
/// for `var`/`function` hoisting: bare blocks and labeled statements, plus the
/// branches of nested `if`. Anything else (a function body, a loop body, etc.)
/// is treated opaquely — if it is itself the dead statement we must keep it, but
/// hoisting out of a nested function does not escape, so we do not need to look
/// inside functions. To stay conservative we simply report `true` for any
/// construct we are unsure about that could contain a hoisted declaration.
fn stmt_has_hoisted_binding(stmt: &Statement) -> bool {
    match stmt {
        Statement::VariableDeclaration(v) => {
            // Only `var` hoists out of blocks; `let`/`const` are block-scoped and
            // do not escape a dead block, but a bare `let`/`const` directly in the
            // SAME list (not nested) would still be a binding we are dropping —
            // however T2/T3 only ever inspects the DEAD branch/tail, where a
            // block-scoped binding cannot be referenced from live code. To be
            // safe we still report `var` as hoisting.
            matches!(v.kind, VariableDeclarationKind::Var)
        }
        Statement::FunctionDeclaration(_) | Statement::ClassDeclaration(_) => true,
        // Bare blocks: `var`/`function` hoist out of them.
        Statement::BlockStatement(b) => b.body.iter().any(stmt_has_hoisted_binding),
        // Labeled statement wraps a single statement.
        Statement::LabeledStatement(l) => stmt_has_hoisted_binding(&l.body),
        // `if`/loops: `var`/`function` declared inside still hoist out.
        Statement::IfStatement(i) => {
            stmt_has_hoisted_binding(&i.consequent)
                || i.alternate
                    .as_ref()
                    .map(stmt_has_hoisted_binding)
                    .unwrap_or(false)
        }
        Statement::ForStatement(f) => {
            let init_var = matches!(
                &f.init,
                Some(ForStatementInit::VariableDeclaration(v))
                    if matches!(v.kind, VariableDeclarationKind::Var)
            );
            init_var || stmt_has_hoisted_binding(&f.body)
        }
        Statement::ForInStatement(f) => stmt_has_hoisted_binding(&f.body),
        Statement::ForOfStatement(f) => stmt_has_hoisted_binding(&f.body),
        Statement::WhileStatement(w) => stmt_has_hoisted_binding(&w.body),
        Statement::DoWhileStatement(d) => stmt_has_hoisted_binding(&d.body),
        Statement::TryStatement(_)
        | Statement::SwitchStatement(_)
        | Statement::WithStatement(_) => {
            // Could contain hoisted `var`/`function`; be conservative.
            true
        }
        _ => false,
    }
}

/// Evaluation / side-effect context. Same conservative knobs as the folding and
/// peephole passes: no known globals, property reads and unknown globals assumed
/// to have side effects — so `may_have_side_effects` is pessimistic (safe).
struct DceCtx<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> GlobalContext<'a> for DceCtx<'a> {
    fn is_global_reference(&self, _reference: &IdentifierReference<'a>) -> bool {
        false
    }
}

impl<'a> MayHaveSideEffectsContext<'a> for DceCtx<'a> {
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

impl<'a> ConstantEvaluationCtx<'a> for DceCtx<'a> {
    fn ast(&self) -> AstBuilder<'a> {
        self.ast
    }
}
