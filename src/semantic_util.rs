//! Semantic helpers: (re)build `Scoping` from a `Program`, plus shared,
//! audited predicates that symbol-touching passes rely on.
//!
//! The driver rebuilds scoping between fixpoint iterations (at O2+) so liveness-
//! sensitive passes (DCE) see up-to-date reference counts after earlier passes
//! mutate the tree.

use std::collections::HashSet;

use oxc_ast::ast::{Expression, Program};
use oxc_semantic::{Scoping, SemanticBuilder};
use oxc_syntax::scope::{ScopeFlags, ScopeId};
use oxc_syntax::symbol::SymbolId;

/// Build a fresh `Scoping` from the current state of `program`.
///
/// Note: `SemanticBuilder::build` borrows the program for `'a`; we immediately
/// extract the owned `Scoping` (which has no lifetime tied to the program) via
/// `into_scoping`, so the borrow ends before the caller mutates the program again.
pub fn rebuild_scoping<'a>(program: &'a Program<'a>) -> Scoping {
    SemanticBuilder::new()
        .build(program)
        .semantic
        .into_scoping()
}

/// True if the program contains a `with` statement OR a direct `eval(...)` call
/// anywhere. Both defeat scope resolution / make reference counts unreliable, so
/// EVERY symbol-touching pass (dce, rename, propagation, ...) must bail when true.
///
/// Direct eval is detected as a bare-identifier callee named `eval` (not
/// `obj.eval`, not an indirect form). Uses a full `Visit` walk so constructs
/// nested inside any expression / nested function / class are all covered.
pub fn program_has_with_or_eval(program: &Program) -> bool {
    use oxc_ast_visit::Visit;
    let mut d = WithEvalDetector { found: false };
    d.visit_program(program);
    d.found
}

/// Result of scanning a program for writes that monkey-patch host globals.
///
/// * `Names(set)` — the COMPLETE, NAMEABLE set of global root identifier names
///   that are written or shadowed (e.g. `Math` from `Math.floor = f` or
///   `Math = x`). An empty set means NO global is patched. A builtin call/member
///   whose root global name is NOT in this set is still safe to fold.
/// * `All` — a write the detector could not attribute to a specific name (a
///   computed-key write on a global root, `g[expr] = v`). Since we cannot know
///   which global was touched, we must conservatively treat EVERY global as
///   patched (whole-pass bail in pure-eval).
#[derive(Debug, Clone)]
pub enum PatchResult {
    Names(HashSet<String>),
    All,
}

/// Collect the set of global root NAMES that the program writes/shadows, or
/// `PatchResult::All` if a write cannot be attributed to a single name (computed
/// member write on a global root). This is the per-name refinement of
/// `program_writes_global_or_member`: pure-eval blocks folding of a builtin only
/// when its root name is in this set (or the result is `All`).
pub fn collect_patched_global_names(program: &Program, scoping: &Scoping) -> PatchResult {
    use oxc_ast_visit::Visit;
    let mut d = GlobalWriteDetector {
        scoping,
        names: HashSet::new(),
        all: false,
    };
    d.visit_program(program);
    if d.all {
        PatchResult::All
    } else {
        PatchResult::Names(d.names)
    }
}

/// If `expr` is a literal usable as a property key, return its key string.
/// Mirrors the literal-key set used by the dynamic-key bail.
fn literal_key_string(expr: &oxc_ast::ast::Expression) -> Option<String> {
    use oxc_ast::ast::Expression;
    match expr {
        Expression::StringLiteral(s) => Some(s.value.to_string()),
        Expression::NumericLiteral(n) => Some(n.value.to_string()),
        Expression::BooleanLiteral(b) => Some(b.value.to_string()),
        Expression::NullLiteral(_) => Some("null".to_string()),
        Expression::ParenthesizedExpression(p) => literal_key_string(&p.expression),
        _ => None,
    }
}

struct GlobalWriteDetector<'s> {
    scoping: &'s Scoping,
    /// Named global roots that are written/shadowed.
    names: HashSet<String>,
    /// True if a write could not be attributed to a single name (computed-key
    /// write on a global root) — forces the conservative whole-pass bail.
    all: bool,
}

/// Names that, when they resolve to a genuine global (no in-scope binding),
/// refer to the host global object itself. A property write through one of these
/// (`globalThis.Math = 1`, `window["Math"] = 1`) replaces the GLOBAL named by the
/// property, not a property of some ordinary object. The pure-eval folder trusts
/// `Math`/`parseInt`/... to be pristine, so such a write must poison the named
/// global (or, when the name is not statically knowable, every global).
const GLOBAL_OBJECT_ALIASES: &[&str] = &["globalThis", "window", "self", "global", "frames"];

/// Names of mutators that, when passed a global (or global-object-alias member)
/// as their FIRST argument, can redefine/replace properties on it. Passing a
/// pristine global to one of these monkey-patches it even though it never appears
/// as an assignment target. We conservatively poison the global's name (or All).
fn is_object_mutator_callee(callee: &oxc_ast::ast::Expression) -> bool {
    use oxc_ast::ast::Expression;
    // Match `Object.<m>` / `Reflect.<m>` static member callees, plus the
    // `__defineGetter__`/`__defineSetter__` prototype methods (any receiver).
    if let Expression::StaticMemberExpression(m) = callee {
        let prop = m.property.name.as_str();
        if let Expression::Identifier(obj) = &m.object {
            let on = obj.name.as_str();
            if on == "Object"
                && matches!(
                    prop,
                    "defineProperty" | "defineProperties" | "assign" | "setPrototypeOf"
                )
            {
                return true;
            }
            if on == "Reflect" && matches!(prop, "defineProperty" | "set" | "setPrototypeOf") {
                return true;
            }
        }
        if matches!(prop, "__defineGetter__" | "__defineSetter__") {
            return true;
        }
    }
    false
}

impl<'s> GlobalWriteDetector<'s> {
    /// Does this identifier reference resolve to no in-scope symbol (a global)?
    fn is_global(&self, id: &oxc_ast::ast::IdentifierReference) -> bool {
        match id.reference_id.get() {
            Some(rid) => self.scoping.get_reference(rid).symbol_id().is_none(),
            None => false,
        }
    }

    /// Is `expr` a reference to the host global OBJECT (a genuine global named
    /// `globalThis`/`window`/`self`/`global`/`frames`)?
    fn is_global_object_alias(&self, expr: &oxc_ast::ast::Expression) -> bool {
        use oxc_ast::ast::Expression;
        match expr {
            Expression::Identifier(id) => {
                self.is_global(id) && GLOBAL_OBJECT_ALIASES.contains(&id.name.as_str())
            }
            Expression::ParenthesizedExpression(p) => self.is_global_object_alias(&p.expression),
            _ => false,
        }
    }

    /// Record a write whose TARGET is the member expression `m` (static form).
    /// `object` is `m.object`, `prop` the static property name.
    fn note_static_member(&mut self, object: &oxc_ast::ast::Expression, prop: &str) {
        // Case 1: `globalThis.Prop = v` (object is a global-object alias). The
        // affected global is `Prop` itself.
        if self.is_global_object_alias(object) {
            self.names.insert(prop.to_string());
            return;
        }
        // Case 2: deeper chain `globalThis.Math.floor = v` — object is
        // `globalThis.Math`; the affected global is `Math` (immediate property of
        // the alias). Find an alias->property step anywhere in the object chain and
        // record that property name.
        if let Some(name) = self.alias_immediate_property(object) {
            self.names.insert(name);
            return;
        }
        // Case 3: ordinary global root (`Math.floor = v`): record the root name.
        self.note_member(object);
    }

    /// If `expr` is (or contains, at its base) a `<global-object-alias>.<Prop>` /
    /// `<alias>["Prop"]` step, return the immediate property name `Prop` (the
    /// global it aliases). Returns None if no alias step is present, or the alias
    /// step uses a non-literal computed key (caller should poison All in that
    /// case via `alias_chain_dynamic`).
    fn alias_immediate_property(&self, expr: &oxc_ast::ast::Expression) -> Option<String> {
        use oxc_ast::ast::Expression;
        match expr {
            Expression::StaticMemberExpression(m) => {
                if self.is_global_object_alias(&m.object) {
                    Some(m.property.name.to_string())
                } else {
                    self.alias_immediate_property(&m.object)
                }
            }
            Expression::ComputedMemberExpression(m) => {
                if self.is_global_object_alias(&m.object) {
                    literal_key_string(&m.expression)
                } else {
                    self.alias_immediate_property(&m.object)
                }
            }
            Expression::ParenthesizedExpression(p) => self.alias_immediate_property(&p.expression),
            _ => None,
        }
    }

    /// True if `expr`'s member chain contains a global-object-alias step whose key
    /// is a NON-literal computed key (`globalThis[k]....`), which can name an
    /// arbitrary global — forces `All`.
    fn alias_chain_dynamic(&self, expr: &oxc_ast::ast::Expression) -> bool {
        use oxc_ast::ast::Expression;
        match expr {
            Expression::StaticMemberExpression(m) => self.alias_chain_dynamic(&m.object),
            Expression::ComputedMemberExpression(m) => {
                if self.is_global_object_alias(&m.object)
                    && literal_key_string(&m.expression).is_none()
                {
                    return true;
                }
                self.alias_chain_dynamic(&m.object)
            }
            Expression::ParenthesizedExpression(p) => self.alias_chain_dynamic(&p.expression),
            _ => false,
        }
    }

    /// The root object identifier of a (possibly chained) member expression.
    /// Also reports whether the chain contained a COMPUTED member with a
    /// non-literal key. A computed non-literal key on a global root (e.g.
    /// `globalThis[k] = v`) could redefine an arbitrarily-named global, so the
    /// caller must conservatively poison every name (`All`).
    fn member_root_ident<'a>(
        expr: &'a oxc_ast::ast::Expression,
    ) -> Option<(&'a oxc_ast::ast::IdentifierReference<'a>, bool)> {
        use oxc_ast::ast::Expression;
        match expr {
            Expression::Identifier(id) => Some((id, false)),
            Expression::StaticMemberExpression(m) => Self::member_root_ident(&m.object),
            Expression::ComputedMemberExpression(m) => {
                let dynamic = !matches!(
                    &m.expression,
                    Expression::StringLiteral(_)
                        | Expression::NumericLiteral(_)
                        | Expression::BooleanLiteral(_)
                        | Expression::NullLiteral(_)
                );
                Self::member_root_ident(&m.object).map(|(root, d)| (root, d || dynamic))
            }
            Expression::ParenthesizedExpression(p) => Self::member_root_ident(&p.expression),
            _ => None,
        }
    }

    /// Record a direct write to a global identifier by name.
    fn note_global_ident(&mut self, id: &oxc_ast::ast::IdentifierReference) {
        if self.is_global(id) {
            self.names.insert(id.name.to_string());
        }
    }

    /// Record a write to a member expression. If rooted at a global, record that
    /// root's name; if the path crossed a computed non-literal key, poison all.
    /// (Plain `Math.floor = f` shape — `object` = `Math`.)
    fn note_member(&mut self, object: &oxc_ast::ast::Expression) {
        if let Some((root, dynamic)) = Self::member_root_ident(object) {
            if self.is_global(root) {
                if dynamic {
                    // Computed non-literal key on a global root: cannot name the
                    // affected global. Conservatively poison every global.
                    self.all = true;
                } else {
                    self.names.insert(root.name.to_string());
                }
            }
        }
    }

    /// Record a member WRITE whose member-`object` is `object` and (for a computed
    /// write) whose own key is `own_key`. Handles three shapes uniformly:
    ///   * `globalThis.X = v` / `globalThis["X"] = v` — the affected global is the
    ///     LEAF property `X`, not the alias root.
    ///   * `globalThis.Math.floor = v` — the affected global is `Math` (the
    ///     immediate property of the alias), found inside the object chain.
    ///   * `Math.floor = v` — ordinary global root; record `Math`.
    ///
    /// A non-literal computed key directly off an alias, or anywhere in an alias
    /// chain, escalates to `All`.
    fn note_member_write(
        &mut self,
        object: &oxc_ast::ast::Expression,
        own_key: &oxc_ast::ast::Expression,
    ) {
        // Direct write off a global-object alias: `<alias>["k"] = v`.
        if self.is_global_object_alias(object) {
            match literal_key_string(own_key) {
                Some(name) => {
                    self.names.insert(name);
                }
                None => self.all = true,
            }
            return;
        }
        // A non-literal computed key somewhere in an alias chain (e.g.
        // `globalThis[k].x = v`) cannot be named.
        if self.alias_chain_dynamic(object) {
            self.all = true;
            return;
        }
        // Deeper alias chain: `globalThis.Math.floor = v` -> affected global Math.
        if let Some(name) = self.alias_immediate_property(object) {
            self.names.insert(name);
            return;
        }
        // Ordinary global root.
        self.note_member(object);
    }

    /// Flag a write whose target is a global identifier or a member expression
    /// rooted at a global identifier (or global-object alias).
    fn check_target(&mut self, target: &oxc_ast::ast::AssignmentTarget) {
        use oxc_ast::ast::{AssignmentTarget, AssignmentTargetProperty};
        match target {
            AssignmentTarget::AssignmentTargetIdentifier(id) => self.note_global_ident(id),
            AssignmentTarget::StaticMemberExpression(m) => {
                self.note_static_member(&m.object, m.property.name.as_str())
            }
            AssignmentTarget::ComputedMemberExpression(m) => {
                self.note_member_write(&m.object, &m.expression)
            }
            // DESTRUCTURING targets: a global member can be written inside an array
            // or object assignment pattern (e.g. `[Math.floor] = [f]`). Recurse.
            AssignmentTarget::ArrayAssignmentTarget(a) => {
                for el in a.elements.iter().flatten() {
                    self.check_maybe_default(el);
                }
                if let Some(rest) = &a.rest {
                    self.check_target(&rest.target);
                }
            }
            AssignmentTarget::ObjectAssignmentTarget(o) => {
                for p in &o.properties {
                    match p {
                        AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(id) => {
                            self.note_global_ident(&id.binding);
                        }
                        AssignmentTargetProperty::AssignmentTargetPropertyProperty(pp) => {
                            self.check_maybe_default(&pp.binding);
                        }
                    }
                }
                if let Some(rest) = &o.rest {
                    self.check_target(&rest.target);
                }
            }
            _ => {}
        }
    }

    /// Recurse into an array/object destructuring element which may carry a
    /// default (`= expr`); the inner target is what we must inspect.
    fn check_maybe_default(&mut self, el: &oxc_ast::ast::AssignmentTargetMaybeDefault) {
        use oxc_ast::ast::AssignmentTargetMaybeDefault;
        match el {
            AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d) => {
                self.check_target(&d.binding)
            }
            other => {
                if let Some(t) = other.as_assignment_target() {
                    self.check_target(t);
                }
            }
        }
    }

    /// Inspect a CallExpression: a known object-mutator (`Object.defineProperty`,
    /// `Object.assign`, `Reflect.defineProperty`, `__defineGetter__`, ...) called
    /// with a pristine global (or `globalThis.X`) as its FIRST argument patches
    /// that global even though it is never an assignment target.
    fn check_call(&mut self, call: &oxc_ast::ast::CallExpression) {
        use oxc_ast::ast::Expression;
        if !is_object_mutator_callee(&call.callee) {
            return;
        }
        // `__defineGetter__`/`__defineSetter__` are invoked AS a method on the
        // receiver being patched (`Math.__defineGetter__(...)`), so the receiver
        // is the callee's object, not arg0.
        if let Expression::StaticMemberExpression(m) = &call.callee {
            if matches!(
                m.property.name.as_str(),
                "__defineGetter__" | "__defineSetter__"
            ) {
                self.note_global_target_value(&m.object);
                return;
            }
        }
        // Object.*/Reflect.* take the target object as argument 0. A leading
        // spread (`Object.assign(...arr)`) has no nameable first target; ignore it
        // (the value is not a fold-eligible pristine global anyway).
        if let Some(arg0) = call.arguments.first() {
            if let Some(expr) = arg0.as_expression() {
                self.note_global_target_value(expr);
            }
        }
    }

    /// `expr` flows into a mutator as the object being patched. If it is a
    /// pristine global identifier (`Math`), poison that name. If it is a
    /// global-object alias member (`globalThis.Math` / `globalThis["Math"]`),
    /// poison the leaf global. If it is the bare global object (`globalThis`),
    /// poison All (any property could be redefined). Anything else is ignored.
    fn note_global_target_value(&mut self, expr: &oxc_ast::ast::Expression) {
        use oxc_ast::ast::Expression;
        match expr {
            Expression::Identifier(id) => {
                if self.is_global(id) {
                    if GLOBAL_OBJECT_ALIASES.contains(&id.name.as_str()) {
                        // Mutating the global object directly can redefine any
                        // global -> All.
                        self.all = true;
                    } else {
                        self.names.insert(id.name.to_string());
                    }
                }
            }
            Expression::ParenthesizedExpression(p) => self.note_global_target_value(&p.expression),
            Expression::StaticMemberExpression(m) => {
                if self.is_global_object_alias(&m.object) {
                    self.names.insert(m.property.name.to_string());
                } else if let Some(name) = self.alias_immediate_property(expr) {
                    self.names.insert(name);
                } else if self.alias_chain_dynamic(expr) {
                    self.all = true;
                }
            }
            Expression::ComputedMemberExpression(m) => {
                if self.is_global_object_alias(&m.object) {
                    match literal_key_string(&m.expression) {
                        Some(name) => {
                            self.names.insert(name);
                        }
                        None => self.all = true,
                    }
                } else if let Some(name) = self.alias_immediate_property(expr) {
                    self.names.insert(name);
                } else if self.alias_chain_dynamic(expr) {
                    self.all = true;
                }
            }
            _ => {}
        }
    }
}

impl<'a, 's> oxc_ast_visit::Visit<'a> for GlobalWriteDetector<'s> {
    fn visit_assignment_expression(&mut self, it: &oxc_ast::ast::AssignmentExpression<'a>) {
        self.check_target(&it.left);
        oxc_ast_visit::walk::walk_assignment_expression(self, it);
    }

    fn visit_update_expression(&mut self, it: &oxc_ast::ast::UpdateExpression<'a>) {
        use oxc_ast::ast::SimpleAssignmentTarget;
        match &it.argument {
            SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => self.note_global_ident(id),
            SimpleAssignmentTarget::StaticMemberExpression(m) => {
                self.note_static_member(&m.object, m.property.name.as_str())
            }
            SimpleAssignmentTarget::ComputedMemberExpression(m) => {
                self.note_member_write(&m.object, &m.expression)
            }
            _ => {}
        }
        oxc_ast_visit::walk::walk_update_expression(self, it);
    }

    fn visit_call_expression(&mut self, it: &oxc_ast::ast::CallExpression<'a>) {
        self.check_call(it);
        oxc_ast_visit::walk::walk_call_expression(self, it);
    }

    fn visit_for_in_statement(&mut self, it: &oxc_ast::ast::ForInStatement<'a>) {
        use oxc_ast::ast::ForStatementLeft;
        if let ForStatementLeft::AssignmentTargetIdentifier(_)
        | ForStatementLeft::StaticMemberExpression(_)
        | ForStatementLeft::ComputedMemberExpression(_)
        | ForStatementLeft::ArrayAssignmentTarget(_)
        | ForStatementLeft::ObjectAssignmentTarget(_) = &it.left
        {
            if let Some(t) = for_left_as_target(&it.left) {
                self.check_target(t);
            }
        }
        oxc_ast_visit::walk::walk_for_in_statement(self, it);
    }

    fn visit_for_of_statement(&mut self, it: &oxc_ast::ast::ForOfStatement<'a>) {
        if let Some(t) = for_left_as_target(&it.left) {
            self.check_target(t);
        }
        oxc_ast_visit::walk::walk_for_of_statement(self, it);
    }
}

/// Reinterpret a `for (LHS of/in ...)` left as an `AssignmentTarget` when it is
/// not a declaration (`for (Math.floor of xs)`), so `check_target` can inspect it.
fn for_left_as_target<'a, 'b>(
    left: &'b oxc_ast::ast::ForStatementLeft<'a>,
) -> Option<&'b oxc_ast::ast::AssignmentTarget<'a>> {
    use oxc_ast::ast::ForStatementLeft;
    match left {
        ForStatementLeft::VariableDeclaration(_) => None,
        other => other.as_assignment_target(),
    }
}

struct WithEvalDetector {
    found: bool,
}

impl<'a> oxc_ast_visit::Visit<'a> for WithEvalDetector {
    fn visit_with_statement(&mut self, it: &oxc_ast::ast::WithStatement<'a>) {
        self.found = true;
        oxc_ast_visit::walk::walk_with_statement(self, it);
    }

    fn visit_call_expression(&mut self, it: &oxc_ast::ast::CallExpression<'a>) {
        if let Expression::Identifier(id) = &it.callee {
            if id.name == "eval" {
                self.found = true;
            }
        }
        oxc_ast_visit::walk::walk_call_expression(self, it);
    }
}

/// A binding is *single-assignment* (immutable for our purposes) when it is a
/// `const` OR is never written/reassigned anywhere. `symbol_is_mutated` already
/// short-circuits `true` for `ConstVariable` and otherwise reports whether any
/// resolved reference is a write, so this is exactly `!symbol_is_mutated`.
pub fn symbol_is_single_assignment(symbol_id: SymbolId, scoping: &Scoping) -> bool {
    !scoping.symbol_is_mutated(symbol_id)
}

/// True if any resolved reference to `symbol_id` lives inside a descendant
/// FUNCTION scope distinct from the symbol's own declaring function scope — i.e.
/// the binding is captured by a closure. Such a binding must not be inlined /
/// moved / propagated across the closure boundary, because the closure could
/// observe a different value/timing.
///
/// We compute the symbol's enclosing function scope (the nearest ancestor-or-self
/// scope with `ScopeFlags::Function`, defaulting to the root) and, for each
/// reference, walk up from the reference's scope to its enclosing function scope.
/// If they differ, the reference is in a nested function => closed over.
pub fn symbol_is_closed_over(symbol_id: SymbolId, scoping: &Scoping) -> bool {
    let decl_scope = scoping.symbol_scope_id(symbol_id);
    let decl_fn = enclosing_function_scope(decl_scope, scoping);
    for reference in scoping.get_resolved_references(symbol_id) {
        let ref_fn = enclosing_function_scope(reference.scope_id(), scoping);
        if ref_fn != decl_fn {
            return true;
        }
    }
    false
}

/// True if this program is an ES MODULE per the AUTHORITATIVE post-parse
/// `program.source_type`. After parsing, oxc resolves `Unambiguous` source
/// (`.js/.ts`) to Module iff the file actually contains import/export syntax,
/// else to Script; `.mjs/.mts` are always Module, `.cjs/.cts` always Script.
///
/// This is the single gate for "root-scope lexical optimization is sound":
/// in a module, top-level `const`/`let` are NOT properties of the global object
/// and are NOT visible to sibling scripts, so propagating/folding/eliminating
/// them within the module is observationally invisible to anything outside it.
/// MUST be read from `program.source_type` (post-parse) — NOT the driver's
/// pre-parse `SourceType::from_path` value, which is still `Unambiguous`.
pub fn is_es_module(program: &Program) -> bool {
    program.source_type.is_module()
}

/// Collect the symbol ids of every TOP-LEVEL exported binding in `program`.
///
/// An exported binding's value is observable by importing modules through the
/// live export slot, so passes that REMOVE or MOVE a binding (inlining, DCE T4,
/// dead-store) must never touch an exported symbol — even in a module. (Reads of
/// an exported `const` may still be propagated within the module: that replaces
/// uses without disturbing the export slot, which keeps holding the same value.)
///
/// Shapes collected:
///   * `export const x = ...;` / `export let x = ...;` / `export function f(){}`
///     / `export class C {}` — the declared `BindingIdentifier`(s).
///   * `export default function f(){}` / `export default class C {}` — the name
///     when present.
///   * `export { local as pub };` — the LOCAL symbol referenced by the specifier.
///   * `export { x } from "..."` (re-export) — no local binding exists, skipped.
pub fn collect_exported_symbols(program: &Program, scoping: &Scoping) -> HashSet<SymbolId> {
    use oxc_ast::ast::{
        BindingPattern, Declaration, ExportDefaultDeclarationKind, ModuleExportName, Statement,
    };

    let mut out = HashSet::new();

    fn collect_pattern(pat: &BindingPattern, out: &mut HashSet<SymbolId>) {
        match pat {
            BindingPattern::BindingIdentifier(id) => {
                if let Some(sym) = id.symbol_id.get() {
                    out.insert(sym);
                }
            }
            BindingPattern::ObjectPattern(o) => {
                for p in &o.properties {
                    collect_pattern(&p.value, out);
                }
                if let Some(rest) = &o.rest {
                    collect_pattern(&rest.argument, out);
                }
            }
            BindingPattern::ArrayPattern(a) => {
                for el in a.elements.iter().flatten() {
                    collect_pattern(el, out);
                }
                if let Some(rest) = &a.rest {
                    collect_pattern(&rest.argument, out);
                }
            }
            BindingPattern::AssignmentPattern(ap) => {
                collect_pattern(&ap.left, out);
            }
        }
    }

    fn collect_decl(decl: &Declaration, out: &mut HashSet<SymbolId>) {
        match decl {
            Declaration::VariableDeclaration(v) => {
                for d in &v.declarations {
                    collect_pattern(&d.id, out);
                }
            }
            Declaration::FunctionDeclaration(f) => {
                if let Some(id) = &f.id {
                    if let Some(sym) = id.symbol_id.get() {
                        out.insert(sym);
                    }
                }
            }
            Declaration::ClassDeclaration(c) => {
                if let Some(id) = &c.id {
                    if let Some(sym) = id.symbol_id.get() {
                        out.insert(sym);
                    }
                }
            }
            _ => {}
        }
    }

    for stmt in &program.body {
        match stmt {
            Statement::ExportNamedDeclaration(e) => {
                if let Some(decl) = &e.declaration {
                    collect_decl(decl, &mut out);
                }
                // `export { local as pub }` with no `from` clause: resolve each
                // local specifier name to its symbol so we keep that binding.
                if e.source.is_none() {
                    for spec in &e.specifiers {
                        if let ModuleExportName::IdentifierReference(idref) = &spec.local {
                            if let Some(rid) = idref.reference_id.get() {
                                if let Some(sym) = scoping.get_reference(rid).symbol_id() {
                                    out.insert(sym);
                                }
                            }
                        }
                    }
                }
            }
            Statement::ExportDefaultDeclaration(e) => {
                if let ExportDefaultDeclarationKind::FunctionDeclaration(f) = &e.declaration {
                    if let Some(id) = &f.id {
                        if let Some(sym) = id.symbol_id.get() {
                            out.insert(sym);
                        }
                    }
                } else if let ExportDefaultDeclarationKind::ClassDeclaration(c) = &e.declaration {
                    if let Some(id) = &c.id {
                        if let Some(sym) = id.symbol_id.get() {
                            out.insert(sym);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    out
}

/// REMOVAL/MOVE predicate: a binding may be inlined / DCE-removed / dead-stored
/// at the root scope ONLY in an ES module AND only when it is NOT exported.
/// (For non-root symbols this is irrelevant — callers gate root specially.)
///
/// This returns true when the symbol IS a root-scope symbol that is now safe to
/// touch (module + non-exported). Callers use it to flip their existing
/// `== root_scope_id()` bail: bail only when the symbol is root AND this returns
/// false (script root, or exported module-root binding).
pub fn is_root_optimizable(
    symbol_id: SymbolId,
    scoping: &Scoping,
    is_module: bool,
    exported: &HashSet<SymbolId>,
) -> bool {
    is_module
        && scoping.symbol_scope_id(symbol_id) == scoping.root_scope_id()
        && !exported.contains(&symbol_id)
}

/// PROPAGATION predicate: a root-scope binding's READS may be replaced in an ES
/// module even if the binding is exported (the export slot is untouched; reads
/// are replaced by an equal literal / copy). Only requires module + root.
pub fn is_root_propagatable(symbol_id: SymbolId, scoping: &Scoping, is_module: bool) -> bool {
    is_module && scoping.symbol_scope_id(symbol_id) == scoping.root_scope_id()
}

/// Whole-program analysis: collect the symbol ids of bindings that PROVABLY hold
/// a finite NUMBER value at every program point (and therefore can never be an
/// object with a `valueOf`/`toString`/`Symbol.toPrimitive` hook).
///
/// WHY NUMBER-ONLY: the coercion-safety model for broadening CSE to the coercing
/// operators (`+ - * / % ** & | ^ << >> >>> < > <= >=`) requires that evaluating
/// each operand triggers ZERO ToPrimitive/ToNumber side effects AND that the
/// operator itself cannot throw based on operand TYPE. Restricting to provably-
/// finite-NUMBER operands gives both: numbers are already primitive (no coercion
/// hook runs), `+` over two numbers is numeric addition (no string/number
/// ambiguity), and number-op-number never throws (unlike BigInt mixed with
/// Number). The operator's internal coercion becomes a pure value computation, so
/// collapsing N evaluations to 1 changes neither observable coercions (there are
/// none) nor the value.
///
/// A symbol is provably-number iff (fixpoint):
///   * it is `const`/`let` and NEVER mutated (`!symbol_is_mutated`), AND
///   * it has exactly ONE declarator with an initializer, AND
///   * that initializer is a `provably-number EXPRESSION` per the rules below,
///     where identifier operands must already be in the provably-number set.
///
/// Excluded by construction: parameters (no const/let declarator init found),
/// `var` (hoisted `undefined` phase is not a finite number — and we never add it),
/// members/calls/globals (an init like `obj.x` or `f()` could be an object).
///
/// Bails to EMPTY on `with`/`eval` (a binding's init could be reassigned via eval,
/// defeating the never-mutated proof).
pub fn collect_provably_number_symbols(program: &Program, scoping: &Scoping) -> HashSet<SymbolId> {
    use oxc_ast::ast::{BindingPattern, VariableDeclarationKind};

    let mut result: HashSet<SymbolId> = HashSet::new();
    if program_has_with_or_eval(program) {
        return result;
    }

    // Gather (symbol_id, init-expression) for every never-mutated const/let
    // declarator with a simple BindingIdentifier and an initializer, anywhere in
    // the program. We walk the full AST via a Visit so nested-function locals are
    // included too.
    // Collect (symbol_id, *const init-expression) for every never-mutated const/
    // let declarator with a simple BindingIdentifier and an initializer. We store
    // RAW POINTERS because the borrow-checker cannot tie the inner `&it`-borrowed
    // init reference to the arena lifetime; this analysis is strictly READ-ONLY
    // and the program (and its arena) outlives this function, so dereferencing the
    // pointers below is sound (no mutation occurs between collection and use).
    use oxc_ast_visit::Visit;
    struct DeclCollector<'b, 's> {
        scoping: &'s Scoping,
        decls: Vec<(SymbolId, *const Expression<'b>)>,
    }
    impl<'b, 's> Visit<'b> for DeclCollector<'b, 's> {
        fn visit_variable_declaration(&mut self, it: &oxc_ast::ast::VariableDeclaration<'b>) {
            if matches!(
                it.kind,
                VariableDeclarationKind::Const | VariableDeclarationKind::Let
            ) {
                for d in &it.declarations {
                    if let BindingPattern::BindingIdentifier(id) = &d.id {
                        if let (Some(sym), Some(init)) = (id.symbol_id.get(), d.init.as_ref()) {
                            if !self.scoping.symbol_is_mutated(sym) {
                                self.decls.push((sym, init as *const Expression));
                            }
                        }
                    }
                }
            }
            oxc_ast_visit::walk::walk_variable_declaration(self, it);
        }
    }
    let mut dc = DeclCollector {
        scoping,
        decls: Vec::new(),
    };
    dc.visit_program(program);

    // Fixpoint: repeatedly admit declarators whose init is provably-number given
    // the current set, until no change.
    loop {
        let mut added = false;
        for (sym, init) in &dc.decls {
            if result.contains(sym) {
                continue;
            }
            // SAFETY: read-only analysis; the program/arena outlives this call and
            // is not mutated here, so the pointer is valid for this deref.
            let init: &Expression = unsafe { &**init };
            if expr_is_provably_number(init, scoping, &result) {
                result.insert(*sym);
                added = true;
            }
        }
        if !added {
            break;
        }
    }
    result
}

/// Is `expr` provably a finite NUMBER value, assuming `prim` already lists symbols
/// known provably-number? Pure structural recursion over the safe operator set;
/// any unrecognized node (member/call/global/this/template/etc.) returns false.
pub fn expr_is_provably_number(
    expr: &Expression,
    scoping: &Scoping,
    prim: &HashSet<SymbolId>,
) -> bool {
    use oxc_syntax::operator::{BinaryOperator, UnaryOperator};
    match expr {
        Expression::NumericLiteral(n) => n.value.is_finite(),
        Expression::ParenthesizedExpression(p) => {
            expr_is_provably_number(&p.expression, scoping, prim)
        }
        Expression::Identifier(id) => match id.reference_id.get() {
            Some(rid) => match scoping.get_reference(rid).symbol_id() {
                Some(sym) => prim.contains(&sym),
                None => false, // global / unresolved
            },
            None => false,
        },
        Expression::UnaryExpression(u) => {
            // `-x`/`+x`/`~x` over a provably-number argument yields a number and
            // runs no coercion hook (argument is already a number). `!`/typeof/void
            // do not yield numbers, so they are not provably-number here.
            if !matches!(
                u.operator,
                UnaryOperator::UnaryNegation | UnaryOperator::UnaryPlus | UnaryOperator::BitwiseNot
            ) {
                return false;
            }
            expr_is_provably_number(&u.argument, scoping, prim)
        }
        Expression::BinaryExpression(b) => {
            // Arithmetic + bitwise over two provably-number operands yields a
            // number with no coercion and no type-based throw. Relational/equality
            // yield booleans (not numbers), so excluded from THIS predicate.
            if !matches!(
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
            ) {
                return false;
            }
            expr_is_provably_number(&b.left, scoping, prim)
                && expr_is_provably_number(&b.right, scoping, prim)
        }
        _ => false,
    }
}

/// THROW-SAFETY predicate for MOVING or DELETING an initializer expression.
///
/// `may_have_side_effects` treats a bare identifier read as effect-free, but
/// reading an identifier can still THROW (an observable effect):
///   * a lexical (`const`/`let`/class/import) binding read while still in its
///     Temporal Dead Zone throws `ReferenceError: Cannot access 'x' before
///     initialization`;
///   * an undeclared global name read throws `ReferenceError: x is not defined`.
///
/// Passes that DELETE a declarator (DCE T4) or MOVE its initializer to a later
/// program point (inlining / propagation copy-prop) must not silently drop such a
/// throw. This returns `true` only when evaluating `init` at the program point of
/// the declarator at `declarator_start` is provably THROWLESS w.r.t. identifier
/// reads: every free identifier read resolves to a binding that is guaranteed
/// already-initialized at that point —
///   * `var` / function-scoped / hoisted function bindings are initialized at
///     hoist time (a read yields `undefined` / the function, never a throw);
///   * a lexical binding (or any other resolved symbol) is safe only when its
///     declaration is textually BEFORE `declarator_start` (so its initializer has
///     already run by the time we reach this point in straight-line order);
///   * a read resolving to NO symbol (a global / unresolved name) is REJECTED:
///     we cannot prove the global exists, so the read may throw a ReferenceError.
///
/// Combine with `!init.may_have_side_effects(...)` at the call site to cover
/// member reads (getters), calls, etc.; this predicate only closes the
/// identifier-read throw hole that `may_have_side_effects` misses.
pub fn init_reads_are_throwless(
    init: &Expression,
    scoping: &Scoping,
    declarator_start: u32,
) -> bool {
    use oxc_ast_visit::Visit;
    use oxc_syntax::symbol::SymbolFlags;

    struct Scan<'s> {
        scoping: &'s Scoping,
        declarator_start: u32,
        safe: bool,
    }
    impl<'a> Visit<'a> for Scan<'_> {
        fn visit_identifier_reference(&mut self, it: &oxc_ast::ast::IdentifierReference<'a>) {
            if !self.safe {
                return;
            }
            let Some(rid) = it.reference_id.get() else {
                self.safe = false;
                return;
            };
            let reference = self.scoping.get_reference(rid);
            if !reference.is_read() {
                return;
            }
            match reference.symbol_id() {
                // Resolves to no in-scope binding: a global / unresolved read that
                // may throw `ReferenceError: x is not defined`. Reject.
                None => self.safe = false,
                Some(sym) => {
                    let flags = self.scoping.symbol_flags(sym);
                    // var / function-scoped / function-declaration bindings are
                    // hoisted and initialized at hoist time; reading them never
                    // throws regardless of textual position.
                    let hoisted_initialized = flags
                        .intersects(SymbolFlags::FunctionScopedVariable | SymbolFlags::Function);
                    if hoisted_initialized {
                        return;
                    }
                    // Any other (lexical / param / import / class) binding is safe
                    // to read here only if its declaration is textually before this
                    // declarator (so its init has already executed; no TDZ).
                    if self.scoping.symbol_span(sym).start >= self.declarator_start {
                        self.safe = false;
                    }
                }
            }
        }
    }

    let mut scan = Scan {
        scoping,
        declarator_start,
        safe: true,
    };
    scan.visit_expression(init);
    scan.safe
}

/// Nearest ancestor-or-self scope that is a function scope; falls back to the
/// outermost scope walked (module/global) if none is a function scope.
fn enclosing_function_scope(scope_id: ScopeId, scoping: &Scoping) -> ScopeId {
    if scoping.scope_flags(scope_id).contains(ScopeFlags::Function) {
        return scope_id;
    }
    let mut last = scope_id;
    for anc in scoping.scope_ancestors(scope_id) {
        last = anc;
        if scoping.scope_flags(anc).contains(ScopeFlags::Function) {
            return anc;
        }
    }
    last
}
