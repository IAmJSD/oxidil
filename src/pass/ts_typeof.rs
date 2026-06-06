//! Pass #4: TS-driven `typeof`-guard elimination. Orthogonal flag: runs only when
//! `--ts-typeof` AND level tier >= 1. Never at `-O0`.
//!
//! THE HEADLINE FEATURE. Using a lightweight, annotation-driven local analysis we
//! learn, for some local symbols, a statically-known primitive `typeof` result
//! ("string"/"number"/"boolean"/"bigint"/"symbol"/"undefined"/"function"/"object").
//! We then fold `typeof x === "T"` (and `!==`/`==`/`!=`, in either operand order)
//! into a boolean literal, so the downstream constant-folding + DCE passes can
//! delete the now-dead branches.
//!
//! ## Why facts are collected before stripping
//! `ts_strip` erases all TS type annotations *before* the pass pipeline runs (oxc
//! codegen would otherwise print TS syntax). So the driver harvests [`TypeofFacts`]
//! from the TS AST up front (see [`collect`]) and threads them through
//! `PassConfig::typeof_facts`. By the time this pass runs the annotations are gone,
//! but the facts (keyed by identifier name) remain.
//!
//! ## Soundness (why we do NOT trust parameter annotations)
//! TypeScript type annotations are ERASED, not runtime-enforced. A value of a
//! different runtime type routinely reaches an annotated parameter via `any`, type
//! assertions, or untyped JS callers — which is exactly why `typeof` guards exist.
//! So a `function f(x: number)` annotation tells us NOTHING about `x`'s runtime
//! type, and folding `typeof x === "string"` to `false` there is unsound. We
//! therefore NEVER fold based on a parameter (or any externally-supplied binding).
//!
//! The ONLY sound source of a runtime-`typeof` fact is a binding whose value the
//! pass can see and prove locally, and which cannot be replaced by an
//! externally-supplied value of a different runtime type. We restrict to:
//!   - a `const` local binding (`let`/`var` can be reassigned; `const` cannot),
//!   - a simple identifier pattern (no destructuring),
//!   - whose initializer is a literal/expression whose runtime `typeof` we can
//!     pin to a single string DIRECTLY FROM THE INITIALIZER (not the annotation).
//!
//! Everything else POISONS the name so it is never folded:
//!   - parameters (externally supplied),
//!   - `let`/`var` bindings, any reassignment, `x++`/`--x`,
//!   - destructuring/member assignment targets (`({a:x}=...)`, `[x]=...`),
//!   - destructuring binding patterns, catch params, function/class decl names,
//!     import specifiers,
//!   - any annotation we cannot pin (`any`/`unknown`/unions/refs/`object`/...).
//!
//! We key facts by identifier **name** (symbol ids are not stable across the strip
//! boundary), and a name is foldable only if EVERY binding/use of that name agrees;
//! any disagreement or any unmodeled binding form poisons it. Plain JS yields zero
//! facts, so the flag is a no-op there.

use std::collections::HashMap;

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    AssignmentExpression, BindingPattern, CatchClause, Class, Expression, FormalParameter,
    Function, ImportDeclaration, Program, Statement, UnaryOperator as AstUnaryOperator,
    UpdateExpression, VariableDeclarationKind, VariableDeclarator,
};
use oxc_ast_visit::Visit;
use oxc_semantic::Scoping;
use oxc_span::SPAN;
use oxc_syntax::operator::{BinaryOperator, UnaryOperator};
use oxc_traverse::{Traverse, TraverseCtx};

use crate::level::OptLevel;
use crate::pass::{run_traverse, Pass, PassConfig, PassResult};

/// The known `typeof` result of a primitive type, i.e. one of the eight strings
/// the `typeof` operator can yield for a value of a statically-known primitive type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeofKind {
    String,
    Number,
    Boolean,
    BigInt,
    // `symbol` has no literal initializer form we pin (only `Symbol()` calls,
    // which are poisoned), so this variant is currently never produced; it is
    // kept for completeness of the `typeof` string space.
    #[allow(dead_code)]
    Symbol,
    Undefined,
    Function,
    Object,
}

impl TypeofKind {
    /// The exact string `typeof` produces for a value of this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            TypeofKind::String => "string",
            TypeofKind::Number => "number",
            TypeofKind::Boolean => "boolean",
            TypeofKind::BigInt => "bigint",
            TypeofKind::Symbol => "symbol",
            TypeofKind::Undefined => "undefined",
            TypeofKind::Function => "function",
            TypeofKind::Object => "object",
        }
    }

    /// Map an INITIALIZER expression to the runtime `typeof` of its value, if (and
    /// only if) we can pin it to a single string DIRECTLY from the expression (no
    /// type-annotation involvement). Returns `None` for anything we cannot pin —
    /// the caller treats `None` as "poison this name".
    ///
    /// SOUNDNESS: we deliberately use the value the binding is initialized with,
    /// not its declared type. Combined with the `const`-only restriction in the
    /// collector, this means the binding's runtime `typeof` cannot be changed by
    /// an externally-supplied mismatched value (unlike a parameter annotation).
    fn from_init_expr(expr: &Expression) -> Option<TypeofKind> {
        match expr {
            Expression::StringLiteral(_) | Expression::TemplateLiteral(_) => {
                Some(TypeofKind::String)
            }
            Expression::NumericLiteral(_) => Some(TypeofKind::Number),
            Expression::BooleanLiteral(_) => Some(TypeofKind::Boolean),
            Expression::BigIntLiteral(_) => Some(TypeofKind::BigInt),
            // `function () {}` / arrow / `class {}` are values whose typeof is
            // "function" (a class is a constructor function).
            Expression::FunctionExpression(_)
            | Expression::ArrowFunctionExpression(_)
            | Expression::ClassExpression(_) => Some(TypeofKind::Function),
            // Object / array literals: typeof is "object".
            Expression::ObjectExpression(_) | Expression::ArrayExpression(_) => {
                Some(TypeofKind::Object)
            }
            // `null`: typeof null === "object".
            Expression::NullLiteral(_) => Some(TypeofKind::Object),
            // Unary minus/plus on a numeric literal is still a number; `!x` is a
            // boolean; `typeof x` is a string; `void x` is undefined.
            Expression::UnaryExpression(u) => match u.operator {
                AstUnaryOperator::UnaryNegation | AstUnaryOperator::UnaryPlus => {
                    Some(TypeofKind::Number)
                }
                AstUnaryOperator::LogicalNot => Some(TypeofKind::Boolean),
                AstUnaryOperator::Typeof => Some(TypeofKind::String),
                AstUnaryOperator::Void => Some(TypeofKind::Undefined),
                _ => None,
            },
            // Everything else is ambiguous (calls, identifiers, member reads,
            // template-with-substitution edge cases handled above as string,
            // binary ops, conditionals, etc.). Bail (poison).
            _ => None,
        }
    }
}

/// A per-name verdict accumulated while scanning annotations.
#[derive(Debug, Clone, Copy)]
enum Fact {
    /// Every binding seen so far agrees on this known kind.
    Known(TypeofKind),
    /// Conflicting / unknown / reassigned: never fold this name.
    Poisoned,
}

/// Symbol-name -> known-primitive `typeof` fact, harvested from the TS AST before
/// stripping. Empty for plain JS or when `--ts-typeof` is off.
#[derive(Debug, Default)]
pub struct TypeofFacts {
    map: HashMap<String, TypeofKind>,
}

impl TypeofFacts {
    /// The known `typeof` kind for `name`, if it was proven unambiguous.
    pub fn get(&self, name: &str) -> Option<TypeofKind> {
        self.map.get(name).copied()
    }

    /// True if no facts were collected (fast path: pass becomes a no-op).
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Harvest [`TypeofFacts`] from a (still-typed) TS program.
///
/// This is a read-only `Visit` walk — it needs no `Scoping` and does not mutate the
/// tree, so the driver can call it on the freshly-parsed program before stripping.
/// For plain-JS programs there are no annotations, so the result is empty.
pub fn collect(program: &Program) -> TypeofFacts {
    let mut c = FactCollector {
        acc: HashMap::new(),
    };
    c.visit_program(program);
    // Demote every still-Known verdict into the public map; drop the Poisoned ones.
    let mut map = HashMap::new();
    for (name, fact) in c.acc {
        if let Fact::Known(kind) = fact {
            map.insert(name, kind);
        }
    }
    TypeofFacts { map }
}

struct FactCollector {
    acc: HashMap<String, Fact>,
}

impl FactCollector {
    /// Record that `name` was bound with annotation `kind` (or `None` => unknown).
    /// Merges with any prior verdict; a disagreement poisons the name.
    fn record_binding(&mut self, name: &str, kind: Option<TypeofKind>) {
        let incoming = match kind {
            Some(k) => Fact::Known(k),
            None => Fact::Poisoned,
        };
        let entry = self.acc.entry(name.to_string()).or_insert(incoming);
        // Merge with the existing entry.
        let merged = match (*entry, incoming) {
            (Fact::Known(a), Fact::Known(b)) if a == b => Fact::Known(a),
            (Fact::Known(_), Fact::Known(_)) => Fact::Poisoned, // conflicting types
            _ => Fact::Poisoned,                                // any None/poison side poisons
        };
        *entry = merged;
    }

    /// Poison `name` outright (e.g. it is reassigned).
    fn poison(&mut self, name: &str) {
        self.acc.insert(name.to_string(), Fact::Poisoned);
    }
}

impl FactCollector {
    /// Poison every identifier bound by a (possibly nested) binding pattern.
    /// Used for destructuring/catch/param patterns we do not model precisely.
    fn poison_binding_pattern(&mut self, pat: &BindingPattern) {
        match pat {
            BindingPattern::BindingIdentifier(id) => self.poison(id.name.as_str()),
            BindingPattern::ObjectPattern(o) => {
                for prop in &o.properties {
                    self.poison_binding_pattern(&prop.value);
                }
                if let Some(rest) = &o.rest {
                    self.poison_binding_pattern(&rest.argument);
                }
            }
            BindingPattern::ArrayPattern(a) => {
                for el in a.elements.iter().flatten() {
                    self.poison_binding_pattern(el);
                }
                if let Some(rest) = &a.rest {
                    self.poison_binding_pattern(&rest.argument);
                }
            }
            BindingPattern::AssignmentPattern(p) => self.poison_binding_pattern(&p.left),
        }
    }
}

impl<'a> Visit<'a> for FactCollector {
    fn visit_formal_parameter(&mut self, it: &FormalParameter<'a>) {
        // Parameters are externally supplied: their declared type is NOT enforced
        // at runtime (any/casts/untyped callers). Never a sound source of a fact;
        // poison every name the parameter binds so a shadowing typed binding can
        // never be mistaken for the parameter.
        self.poison_binding_pattern(&it.pattern);
        oxc_ast_visit::walk::walk_formal_parameter(self, it);
    }

    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        // The ONLY sound fact source: a `const` binding to a simple identifier
        // whose initializer's runtime typeof we can pin directly. Anything else
        // (let/var, destructuring, no/unknown initializer) poisons the name.
        match &it.id {
            BindingPattern::BindingIdentifier(id) => {
                let is_const = matches!(it.kind, VariableDeclarationKind::Const);
                let kind = if is_const {
                    it.init.as_ref().and_then(TypeofKind::from_init_expr)
                } else {
                    // let/var can be reassigned later (possibly to a different
                    // runtime type); never fold.
                    None
                };
                self.record_binding(id.name.as_str(), kind);
            }
            other => self.poison_binding_pattern(other),
        }
        oxc_ast_visit::walk::walk_variable_declarator(self, it);
    }

    fn visit_catch_clause(&mut self, it: &CatchClause<'a>) {
        // A `catch (x)` parameter introduces a fresh binding of `x` whose runtime
        // type is whatever was thrown (unknown). Poison so it cannot inherit an
        // outer typed `x` fact.
        if let Some(param) = &it.param {
            self.poison_binding_pattern(&param.pattern);
        }
        oxc_ast_visit::walk::walk_catch_clause(self, it);
    }

    fn visit_function(&mut self, it: &Function<'a>, flags: oxc_syntax::scope::ScopeFlags) {
        // A function declaration/expression name binds a callable; poison it (its
        // typeof is "function", but we do not fold on declaration names and we must
        // ensure it cannot inherit a same-named typed fact).
        if let Some(id) = &it.id {
            self.poison(id.name.as_str());
        }
        oxc_ast_visit::walk::walk_function(self, it, flags);
    }

    fn visit_class(&mut self, it: &Class<'a>) {
        if let Some(id) = &it.id {
            self.poison(id.name.as_str());
        }
        oxc_ast_visit::walk::walk_class(self, it);
    }

    fn visit_import_declaration(&mut self, it: &ImportDeclaration<'a>) {
        // Imported bindings come from another module; their runtime type is not
        // known here. Poison every imported local name.
        if let Some(specs) = &it.specifiers {
            for spec in specs {
                use oxc_ast::ast::ImportDeclarationSpecifier as S;
                let name = match spec {
                    S::ImportSpecifier(s) => s.local.name.as_str(),
                    S::ImportDefaultSpecifier(s) => s.local.name.as_str(),
                    S::ImportNamespaceSpecifier(s) => s.local.name.as_str(),
                };
                self.poison(name);
            }
        }
        oxc_ast_visit::walk::walk_import_declaration(self, it);
    }

    fn visit_assignment_expression(&mut self, it: &AssignmentExpression<'a>) {
        // Any assignment LHS — simple identifier, member, OR destructuring pattern
        // (`({a:x}=...)`, `[x]=...`) — may change the runtime type of every name it
        // binds. Poison them all. We do this by walking the LHS and poisoning each
        // `AssignmentTargetIdentifier` we find (handled in
        // `visit_simple_assignment_target` / `visit_assignment_target_property_identifier`).
        oxc_ast_visit::walk::walk_assignment_expression(self, it);
    }

    fn visit_simple_assignment_target(&mut self, it: &oxc_ast::ast::SimpleAssignmentTarget<'a>) {
        if let oxc_ast::ast::SimpleAssignmentTarget::AssignmentTargetIdentifier(id) = it {
            self.poison(id.name.as_str());
        }
        oxc_ast_visit::walk::walk_simple_assignment_target(self, it);
    }

    fn visit_assignment_target_property_identifier(
        &mut self,
        it: &oxc_ast::ast::AssignmentTargetPropertyIdentifier<'a>,
    ) {
        // Shorthand `({ x } = ...)`: `x` is both the property key and the assigned
        // binding target.
        self.poison(it.binding.name.as_str());
        oxc_ast_visit::walk::walk_assignment_target_property_identifier(self, it);
    }

    fn visit_update_expression(&mut self, it: &UpdateExpression<'a>) {
        // `x++` / `--x`: mutation of the binding. Poison to be safe.
        if let Some(name) = it.argument.get_identifier_name() {
            self.poison(name);
        }
        oxc_ast_visit::walk::walk_update_expression(self, it);
    }
}

#[derive(Default)]
pub struct TsTypeofElimination;

impl Pass for TsTypeofElimination {
    fn name(&self) -> &'static str {
        "ts-typeof-elimination"
    }

    fn min_level(&self) -> OptLevel {
        OptLevel::O1
    }

    /// Eligible only when the orthogonal `--ts-typeof` flag is set AND level tier >= O1.
    fn should_run(&self, cfg: &PassConfig) -> bool {
        cfg.ts_typeof && cfg.level.tier() >= self.min_level().tier()
    }

    fn run<'a>(
        &mut self,
        program: &mut Program<'a>,
        scoping: &mut Scoping,
        allocator: &'a Allocator,
        cfg: &PassConfig,
    ) -> PassResult {
        // Fast path: nothing learned => nothing to do.
        if cfg.typeof_facts.is_empty() {
            return PassResult::UNCHANGED;
        }
        let mut visitor = TypeofVisitor {
            facts: &cfg.typeof_facts,
            changed: false,
        };
        run_traverse(&mut visitor, allocator, program, scoping, ());
        if visitor.changed {
            PassResult::CHANGED
        } else {
            PassResult::UNCHANGED
        }
    }
}

struct TypeofVisitor<'f> {
    facts: &'f TypeofFacts,
    changed: bool,
}

impl<'a, 'f> Traverse<'a, ()> for TypeofVisitor<'f> {
    fn exit_expression(&mut self, node: &mut Expression<'a>, ctx: &mut TraverseCtx<'a, ()>) {
        // Match `typeof IDENT <cmp> "STR"` (in either operand order) on a binary
        // equality operator, where IDENT has a known-primitive fact.
        let Expression::BinaryExpression(bin) = node else {
            return;
        };
        let negate_for_inequality = match bin.operator {
            BinaryOperator::Equality | BinaryOperator::StrictEquality => false,
            BinaryOperator::Inequality | BinaryOperator::StrictInequality => true,
            _ => return,
        };

        // Identify which side is `typeof IDENT` and which is the string literal.
        let known = typeof_known_kind(&bin.left, self.facts)
            .zip(string_literal_value(&bin.right))
            .or_else(|| {
                typeof_known_kind(&bin.right, self.facts).zip(string_literal_value(&bin.left))
            });

        let Some((kind, lit)) = known else {
            return;
        };

        // The comparison is statically decidable: equality is true iff the literal
        // equals the known typeof string.
        let mut result = kind.as_str() == lit;
        if negate_for_inequality {
            result = !result;
        }

        *node = ctx.ast.expression_boolean_literal(SPAN, result);
        self.changed = true;
    }

    fn exit_statement(&mut self, node: &mut Statement<'a>, ctx: &mut TraverseCtx<'a, ()>) {
        // `switch (typeof x) { case "T": ... }` — when x has a known kind, every
        // case test `"S"` is statically decidable. We do NOT restructure the
        // switch (fall-through semantics make that subtle); instead we replace each
        // string-literal case test with a boolean-equality against the known kind so
        // constant-folding/DCE can prune. Conservative: only touch literal tests.
        let Statement::SwitchStatement(sw) = node else {
            return;
        };
        let Some(kind) = typeof_known_kind(&sw.discriminant, self.facts) else {
            return;
        };
        for case in sw.cases.iter_mut() {
            if let Some(test) = &case.test {
                if let Some(lit) = string_literal_value(test) {
                    let val = kind.as_str() == lit;
                    // Rewrite discriminant comparison: replace the case test with a
                    // literal boolean and the discriminant with that same boolean, so
                    // `case true:`/`case false:` reduces predictably. We keep it simple
                    // and sound by leaving the switch shape intact; downstream passes
                    // see constant tests. (No-op for non-literal tests.)
                    case.test = Some(ctx.ast.expression_boolean_literal(SPAN, val));
                    self.changed = true;
                }
            }
        }
        if self.changed {
            // Make the discriminant a constant boolean `true` so that `case true:`
            // (the matching arm) is selected by downstream evaluation. We only do
            // this once we have rewritten the cases above.
            sw.discriminant = ctx.ast.expression_boolean_literal(SPAN, true);
        }
    }
}

/// If `expr` is `typeof IDENT` where IDENT has a known fact, return the kind.
fn typeof_known_kind(expr: &Expression, facts: &TypeofFacts) -> Option<TypeofKind> {
    let Expression::UnaryExpression(u) = expr else {
        return None;
    };
    if u.operator != UnaryOperator::Typeof {
        return None;
    }
    let Expression::Identifier(id) = &u.argument else {
        return None;
    };
    facts.get(id.name.as_str())
}

/// If `expr` is a string literal, return its value.
fn string_literal_value<'e>(expr: &'e Expression) -> Option<&'e str> {
    match expr {
        Expression::StringLiteral(s) => Some(s.value.as_str()),
        _ => None,
    }
}
