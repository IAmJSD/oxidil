//! Pass #2: algebraic / peephole simplification (min level O1).
//!
//! In the spirit of GCC's `simplify-rtx`, but SOUND for JavaScript semantics.
//! Every rewrite below is documented with the exact safety condition that makes
//! it observationally equivalent. When a condition cannot be proven, we DO NOT
//! transform (soundness over aggressiveness).
//!
//! Pattern is copied from `constant_fold.rs`: a `Traverse` visitor with a
//! `changed` flag, mutating in `exit_expression` (post-order, so children are
//! already simplified), rebuilding nodes via `ctx.ast` (same arena), and bridged
//! through `run_traverse`. We move inner expressions out with
//! `std::mem::replace(slot, ctx.ast.expression_null_literal(SPAN))`.
//!
//! RULES (each with its soundness condition):
//!   R1  Double negation in boolean context: `!!x` collapses to `x` ONLY when the
//!       `!!x` itself sits where its value is consumed as a boolean (the argument
//!       of another `!`). `!!!x -> !x`. (Bare `!!x -> x` would be UNSOUND: `!!x`
//!       yields a Boolean, `x` may be any type.) We implement the safe subset:
//!       `!(!!x) -> !x` i.e. when a `!` wraps a `!!`, drop one inner pair.
//!   R2  `x ? a : b` with a side-effect-free, constant-boolean test -> the taken
//!       branch. Safe: the test has no side effects and a fixed truthiness.
//!   R3  `x ? a : a -> a` when `x` is side-effect-free. Safe: both branches are
//!       structurally identical and the discarded test cannot be observed.
//!   R4  `&&` / `||` / `??` short-circuit when the LEFT operand is a
//!       side-effect-free constant whose controlling value is known:
//!         - `true && y  -> y`,  `false && y -> false`
//!         - `true || y  -> true`, `false || y -> y`
//!         - `nonnull ?? y -> left`, `null/undefined ?? y -> y`
//!       Safe: left is observably a fixed value with no side effects.
//!   R5  Multiplicative / subtractive identities on KNOWN-NUMBER operands:
//!         - `x * 1 -> x`, `1 * x -> x`     (sound incl. -0: `-0*1 === -0`)
//!         - `x / 1 -> x`                   (sound incl. -0/NaN/Infinity)
//!         - `x - 0 -> x`                   (sound incl. -0: `-0 - 0 === -0`)
//!       Guarded by `value_type(x) == Number` so we never touch string `+`,
//!       BigInt, or coercions. We deliberately DO NOT do `x + 0 -> x`
//!       (UNSOUND for `-0`: `(-0) + 0 === +0 !== -0`) nor `x * 0`
//!       (UNSOUND: `Infinity*0=NaN`, and BigInt throws).
//!
//! Bitwise / shift rewrites are intentionally OMITTED: JS bitwise ops force
//! ToInt32/ToUint32 coercions and the safe-rewrite surface is narrow, so per the
//! spec we leave them untouched.

use oxc_allocator::{Allocator, CloneIn};
use oxc_ast::ast::{Expression, IdentifierReference, Program};
use oxc_ast::AstBuilder;
use oxc_ecmascript::constant_evaluation::{
    ConstantEvaluation, ConstantEvaluationCtx, DetermineValueType,
};
use oxc_ecmascript::side_effects::{
    MayHaveSideEffects, MayHaveSideEffectsContext, PropertyReadSideEffects,
};
use oxc_ecmascript::GlobalContext;
use oxc_semantic::Scoping;
use oxc_span::SPAN;
use oxc_syntax::operator::{BinaryOperator, LogicalOperator, UnaryOperator};
use oxc_traverse::{Traverse, TraverseCtx};

use crate::level::OptLevel;
use crate::pass::{run_traverse, Pass, PassConfig, PassResult};

#[derive(Default)]
pub struct PeepholeAlgebraic;

impl Pass for PeepholeAlgebraic {
    fn name(&self) -> &'static str {
        "peephole-algebraic"
    }

    fn min_level(&self) -> OptLevel {
        OptLevel::O1
    }

    fn run<'a>(
        &mut self,
        program: &mut Program<'a>,
        scoping: &mut Scoping,
        allocator: &'a Allocator,
        _cfg: &PassConfig,
    ) -> PassResult {
        let mut visitor = PeepholeVisitor { changed: false };
        run_traverse(&mut visitor, allocator, program, scoping, ());
        if visitor.changed {
            PassResult::CHANGED
        } else {
            PassResult::UNCHANGED
        }
    }
}

struct PeepholeVisitor {
    changed: bool,
}

impl<'a> Traverse<'a, ()> for PeepholeVisitor {
    fn exit_expression(&mut self, node: &mut Expression<'a>, ctx: &mut TraverseCtx<'a, ()>) {
        let eval_ctx = PeepholeCtx { ast: ctx.ast };

        let replacement = match node {
            Expression::UnaryExpression(_) => try_unary(node, &eval_ctx),
            Expression::ConditionalExpression(_) => try_conditional(node, &eval_ctx),
            Expression::LogicalExpression(_) => try_logical(node, &eval_ctx),
            Expression::BinaryExpression(_) => try_binary(node, &eval_ctx),
            _ => None,
        };

        if let Some(new_expr) = replacement {
            *node = new_expr;
            self.changed = true;
        }
    }
}

/// Move an expression out of a `&mut Expression` slot, leaving a placeholder
/// `null` literal behind (the placeholder is discarded with the old node).
fn take_expr<'a>(slot: &mut Expression<'a>, ast: AstBuilder<'a>) -> Expression<'a> {
    std::mem::replace(slot, ast.expression_null_literal(SPAN))
}

// ---- R1: double-negation in boolean context ------------------------------

/// `!(!!x)` -> `!x`. We only fire when the OUTER node is `!` and its argument is
/// `!!x` (a `!` whose argument is itself a `!`). Collapsing one pair of inner
/// `!!` is sound here because the outer `!` already coerces to boolean, and
/// `!!!x === !x` for every value of `x`.
fn try_unary<'a>(node: &Expression<'a>, ctx: &PeepholeCtx<'a>) -> Option<Expression<'a>> {
    let Expression::UnaryExpression(outer) = node else {
        return None;
    };
    if outer.operator != UnaryOperator::LogicalNot {
        return None;
    }
    // argument must be `!!x`
    let Expression::UnaryExpression(mid) = &outer.argument else {
        return None;
    };
    if mid.operator != UnaryOperator::LogicalNot {
        return None;
    }
    let Expression::UnaryExpression(inner) = &mid.argument else {
        return None;
    };
    if inner.operator != UnaryOperator::LogicalNot {
        return None;
    }
    // We have `! ! ! x`. Rewrite to `! x` by deep-cloning `x` into the same
    // arena and wrapping it in a single `!`.
    let arg = inner.argument.clone_in(ctx.ast.allocator);
    Some(
        ctx.ast
            .expression_unary(SPAN, UnaryOperator::LogicalNot, arg),
    )
}

// ---- R2/R3: conditional simplification ------------------------------------

fn try_conditional<'a>(node: &mut Expression<'a>, ctx: &PeepholeCtx<'a>) -> Option<Expression<'a>> {
    let Expression::ConditionalExpression(cond) = node else {
        return None;
    };

    // R2: constant, side-effect-free test selects a branch.
    if !cond.test.may_have_side_effects(ctx) {
        if let Some(b) = cond.test.evaluate_value_to_boolean(ctx) {
            let chosen = if b {
                &mut cond.consequent
            } else {
                &mut cond.alternate
            };
            return Some(take_expr(chosen, ctx.ast));
        }
    }

    // R3: `x ? a : a -> a` when test is side-effect-free and both branches are
    // structurally identical.
    if !cond.test.may_have_side_effects(ctx)
        && exprs_structurally_equal(&cond.consequent, &cond.alternate)
    {
        return Some(take_expr(&mut cond.consequent, ctx.ast));
    }

    None
}

// ---- R4: logical short-circuit --------------------------------------------

fn try_logical<'a>(node: &mut Expression<'a>, ctx: &PeepholeCtx<'a>) -> Option<Expression<'a>> {
    let Expression::LogicalExpression(logical) = node else {
        return None;
    };

    // Left must be side-effect-free for any of these rewrites: otherwise we
    // would drop an observable evaluation.
    if logical.left.may_have_side_effects(ctx) {
        return None;
    }

    match logical.operator {
        LogicalOperator::And | LogicalOperator::Or => {
            let left_bool = logical.left.evaluate_value_to_boolean(ctx)?;
            match (logical.operator, left_bool) {
                // true && y -> y ; false || y -> y
                (LogicalOperator::And, true) | (LogicalOperator::Or, false) => {
                    Some(take_expr(&mut logical.right, ctx.ast))
                }
                // false && y -> left (the falsy left value) ;
                // true  || y -> left (the truthy left value)
                (LogicalOperator::And, false) | (LogicalOperator::Or, true) => {
                    Some(take_expr(&mut logical.left, ctx.ast))
                }
                _ => unreachable!(),
            }
        }
        LogicalOperator::Coalesce => {
            // `?? y`: if left is provably null/undefined -> y; if provably
            // non-nullish -> left. We use value_type for nullishness.
            let vt = logical.left.value_type(ctx);
            if vt.is_null() || vt.is_undefined() {
                Some(take_expr(&mut logical.right, ctx.ast))
            } else if vt.is_number()
                || vt.is_string()
                || vt.is_boolean()
                || vt.is_bigint()
                || vt.is_object()
            {
                // Definitely non-nullish -> left short-circuits.
                Some(take_expr(&mut logical.left, ctx.ast))
            } else {
                None
            }
        }
    }
}

// ---- R5: numeric identities -----------------------------------------------

fn try_binary<'a>(node: &mut Expression<'a>, ctx: &PeepholeCtx<'a>) -> Option<Expression<'a>> {
    let Expression::BinaryExpression(bin) = node else {
        return None;
    };

    let op = bin.operator;
    // Only multiplicative/subtractive identities are sound without -0/NaN/string
    // hazards. (Addition is excluded: `-0 + 0 === +0`.)
    match op {
        BinaryOperator::Multiplication => {
            // x * 1 -> x   /   1 * x -> x   (x must be known Number)
            if literal_is_number(&bin.right, 1.0) && bin.left.value_type(ctx).is_number() {
                return Some(take_expr(&mut bin.left, ctx.ast));
            }
            if literal_is_number(&bin.left, 1.0) && bin.right.value_type(ctx).is_number() {
                return Some(take_expr(&mut bin.right, ctx.ast));
            }
            None
        }
        BinaryOperator::Division => {
            // x / 1 -> x   (x known Number; -0/NaN/Infinity all preserved)
            if literal_is_number(&bin.right, 1.0) && bin.left.value_type(ctx).is_number() {
                return Some(take_expr(&mut bin.left, ctx.ast));
            }
            None
        }
        BinaryOperator::Subtraction => {
            // x - 0 -> x   (x known Number; `-0 - 0 === -0` is preserved)
            if literal_is_number(&bin.right, 0.0) && bin.left.value_type(ctx).is_number() {
                return Some(take_expr(&mut bin.left, ctx.ast));
            }
            None
        }
        _ => None,
    }
}

/// True if `e` is a numeric literal exactly equal to `target` (bitwise via ==;
/// `0.0 == -0.0` is true in Rust which is fine: we only use targets 0.0 and 1.0
/// and the literal `-0` would be a UnaryExpression, not a NumericLiteral).
fn literal_is_number(e: &Expression, target: f64) -> bool {
    matches!(e, Expression::NumericLiteral(lit) if lit.value == target)
}

/// Conservative structural equality for the `x ? a : a -> a` rule. We only treat
/// trivially-pure, identical leaves as equal (identifiers and primitive
/// literals). Anything more complex returns false (we skip the rewrite).
fn exprs_structurally_equal(a: &Expression, b: &Expression) -> bool {
    match (a, b) {
        (Expression::Identifier(x), Expression::Identifier(y)) => x.name == y.name,
        (Expression::NumericLiteral(x), Expression::NumericLiteral(y)) => {
            x.value == y.value && x.value.to_bits() == y.value.to_bits()
        }
        (Expression::StringLiteral(x), Expression::StringLiteral(y)) => x.value == y.value,
        (Expression::BooleanLiteral(x), Expression::BooleanLiteral(y)) => x.value == y.value,
        (Expression::NullLiteral(_), Expression::NullLiteral(_)) => true,
        _ => false,
    }
}

/// Evaluation/side-effect context. Same conservative knobs as the folding pass:
/// no known globals, property reads & unknown globals assumed effectful.
struct PeepholeCtx<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> GlobalContext<'a> for PeepholeCtx<'a> {
    fn is_global_reference(&self, _reference: &IdentifierReference<'a>) -> bool {
        false
    }
}

impl<'a> MayHaveSideEffectsContext<'a> for PeepholeCtx<'a> {
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

impl<'a> ConstantEvaluationCtx<'a> for PeepholeCtx<'a> {
    fn ast(&self) -> AstBuilder<'a> {
        self.ast
    }
}
