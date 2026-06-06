//! Custom TS -> JS type stripper.
//!
//! Replaces `oxc_transformer` (which does not build on Rust 1.93.1). A `Traverse`
//! impl that:
//!   - drops TS-only statements (`Statement::is_typescript_syntax`) via `enter_statements`,
//!   - unwraps TS-only wrapper expressions (`as`, `satisfies`, `!`, `<T>x`) to their
//!     inner runtime expression.
//!
//! This runs for TS input at EVERY level (including -O0) because oxc_codegen prints
//! TypeScript syntax verbatim. Type annotations on declarations are not separately
//! cleared here: oxc_codegen omits annotations that are not part of runtime output for
//! the constructs we keep, and any remaining TS-only *statements* are removed wholesale.

use oxc_allocator::{Allocator, Vec as ArenaVec};
use oxc_ast::ast::{
    AccessorProperty, ArrowFunctionExpression, CatchParameter, Expression, FormalParameter,
    Function, Program, PropertyDefinition, Statement, VariableDeclarator,
};
use oxc_semantic::Scoping;
use oxc_span::SPAN;
use oxc_traverse::{traverse_mut, Traverse, TraverseCtx};

/// Strip TypeScript-only syntax from `program` in place. Returns true if anything changed.
///
/// `oxc_traverse::traverse_mut` requires the program's scope ids to be populated by
/// `SemanticBuilder` first (it reads `Program::scope_id` etc.), so the caller passes a
/// `Scoping` built from this exact program. ts_strip does not otherwise consult scoping;
/// the returned `Scoping` is discarded (it is stale after we mutate the tree).
pub fn strip<'a>(program: &mut Program<'a>, allocator: &'a Allocator, scoping: Scoping) -> bool {
    let mut v = TsStrip { changed: false };
    let _ = traverse_mut(&mut v, allocator, program, scoping, ());
    v.changed
}

struct TsStrip {
    changed: bool,
}

impl<'a> Traverse<'a, ()> for TsStrip {
    fn enter_statements(
        &mut self,
        node: &mut ArenaVec<'a, Statement<'a>>,
        _ctx: &mut TraverseCtx<'a, ()>,
    ) {
        let before = node.len();
        node.retain(|stmt| !stmt.is_typescript_syntax());
        if node.len() != before {
            self.changed = true;
        }
    }

    fn exit_expression(&mut self, node: &mut Expression<'a>, ctx: &mut TraverseCtx<'a, ()>) {
        // Unwrap TS-only wrapper expressions to their runtime inner expression.
        // Done on EXIT (post-order) so the inner expression has already been visited
        // and any nested TS wrappers are removed before we hoist the inner up.
        let inner: Option<Expression<'a>> = match node {
            Expression::TSAsExpression(e) => Some(std::mem::replace(&mut e.expression, dummy(ctx))),
            Expression::TSSatisfiesExpression(e) => {
                Some(std::mem::replace(&mut e.expression, dummy(ctx)))
            }
            Expression::TSNonNullExpression(e) => {
                Some(std::mem::replace(&mut e.expression, dummy(ctx)))
            }
            Expression::TSTypeAssertion(e) => {
                Some(std::mem::replace(&mut e.expression, dummy(ctx)))
            }
            _ => None,
        };
        if let Some(inner) = inner {
            *node = inner;
            self.changed = true;
        }
    }

    // --- clear TS-only type annotations so codegen emits pure JS ---

    fn enter_variable_declarator(
        &mut self,
        node: &mut VariableDeclarator<'a>,
        _ctx: &mut TraverseCtx<'a, ()>,
    ) {
        if node.type_annotation.take().is_some() {
            self.changed = true;
        }
        // `definite` (the `!` in `let x!: T`) is TS-only.
        if node.definite {
            node.definite = false;
            self.changed = true;
        }
    }

    fn enter_formal_parameter(
        &mut self,
        node: &mut FormalParameter<'a>,
        _ctx: &mut TraverseCtx<'a, ()>,
    ) {
        if node.type_annotation.take().is_some() {
            self.changed = true;
        }
        if node.optional {
            node.optional = false;
            self.changed = true;
        }
    }

    fn enter_function(&mut self, node: &mut Function<'a>, _ctx: &mut TraverseCtx<'a, ()>) {
        if node.return_type.take().is_some() {
            self.changed = true;
        }
        if node.type_parameters.take().is_some() {
            self.changed = true;
        }
        if node.this_param.take().is_some() {
            self.changed = true;
        }
    }

    fn enter_arrow_function_expression(
        &mut self,
        node: &mut ArrowFunctionExpression<'a>,
        _ctx: &mut TraverseCtx<'a, ()>,
    ) {
        if node.return_type.take().is_some() {
            self.changed = true;
        }
        if node.type_parameters.take().is_some() {
            self.changed = true;
        }
    }

    fn enter_property_definition(
        &mut self,
        node: &mut PropertyDefinition<'a>,
        _ctx: &mut TraverseCtx<'a, ()>,
    ) {
        if node.type_annotation.take().is_some() {
            self.changed = true;
        }
    }

    fn enter_accessor_property(
        &mut self,
        node: &mut AccessorProperty<'a>,
        _ctx: &mut TraverseCtx<'a, ()>,
    ) {
        if node.type_annotation.take().is_some() {
            self.changed = true;
        }
    }

    fn enter_catch_parameter(
        &mut self,
        node: &mut CatchParameter<'a>,
        _ctx: &mut TraverseCtx<'a, ()>,
    ) {
        if node.type_annotation.take().is_some() {
            self.changed = true;
        }
    }
}

/// A throwaway expression to swap in while moving the real inner expression out.
fn dummy<'a>(ctx: &mut TraverseCtx<'a, ()>) -> Expression<'a> {
    ctx.ast.expression_null_literal(SPAN)
}
