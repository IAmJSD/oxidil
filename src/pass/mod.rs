//! The `Pass` trait, the per-invocation config, and the central pass registry.
//!
//! VERIFIED against oxc_traverse 0.133.0 (`Traverse<'a, State>`, `traverse_mut`,
//! `TraverseCtx { pub ast: AstBuilder<'a>, scoping()/scoping_mut(), parent()/ancestors() }`)
//! and oxc_semantic 0.133.0 `Scoping`.

use std::collections::HashSet;
use std::rc::Rc;

use oxc_allocator::Allocator;
use oxc_ast::ast::Program;
use oxc_semantic::Scoping;
use oxc_syntax::symbol::SymbolId;

use crate::level::OptLevel;

pub mod array_construct;
pub mod constant_fold;
pub mod control_flow;
pub mod cse_gvn;
pub mod dce;
pub mod dead_param;
pub mod dead_store;
pub mod inlining;
pub mod licm;
pub mod object_construct;
pub mod param_scalarize;
pub mod peephole;
pub mod propagation;
pub mod pure_eval;
pub mod rename;
pub mod ts_typeof;

pub use ts_typeof::TypeofFacts;

/// Outcome of running one pass once over the whole Program.
#[derive(Debug, Default, Clone, Copy)]
pub struct PassResult {
    /// True if the pass mutated the AST. Drives the driver's fixpoint loop.
    pub changed: bool,
}

impl PassResult {
    pub const UNCHANGED: PassResult = PassResult { changed: false };
    pub const CHANGED: PassResult = PassResult { changed: true };
}

/// Knobs handed to every pass invocation. `ts_typeof` is the orthogonal flag.
///
/// NOTE: this is `Clone` (not `Copy`) because it carries `typeof_facts`, the
/// type facts harvested by the driver from the TS AST *before* type-stripping
/// erases the annotations. It is constructed once and only ever borrowed (`&`),
/// so dropping `Copy` has no call-site impact.
#[derive(Debug, Clone)]
pub struct PassConfig {
    pub level: OptLevel,
    pub ts_typeof: bool,
    /// size-oriented mode (`-Os`): enables rename + compact codegen at the driver level.
    pub size: bool,
    /// Symbol-name -> known-primitive facts collected from TS annotations before
    /// stripping. Consumed by the `ts-typeof-elimination` pass. Empty for plain JS
    /// or when `--ts-typeof` is off. `Rc` so cloning the config stays cheap.
    pub typeof_facts: Rc<TypeofFacts>,
    /// True iff this compilation unit is an ES MODULE (per `program.source_type`
    /// resolved AFTER parse — see `semantic_util::is_es_module`). Gates the
    /// root-scope lexical optimizations (propagation/inlining/DCE of module
    /// top-level `const`/`let`). Always false for scripts (`.js/.cjs` with no
    /// import/export).
    pub is_module: bool,
    /// Symbol ids of top-level EXPORTED bindings (empty for scripts). These must
    /// never be removed/moved by inlining/DCE/dead-store, though their reads may
    /// still be propagated within the module. `Rc` so cloning the config is cheap.
    pub exported: Rc<HashSet<SymbolId>>,
}

/// The interface every optimization pass implements.
///
/// A pass borrows the arena allocator (for building replacement nodes via `ctx.ast`)
/// and operates over the whole `Program`. Scoping (symbols/scopes/reference counts) is
/// supplied by `&mut` and may be consumed/returned because oxc's `traverse_mut` takes
/// `Scoping` by value and returns it (use [`run_traverse`] to bridge that). The registry
/// gates passes purely via `min_level()` / `os_only()`.
pub trait Pass {
    /// Stable kebab-case id, used by `--enable-<id>`/`--disable-<id>` and diagnostics.
    fn name(&self) -> &'static str;

    /// Minimum `-O` level (tier) at which this pass is eligible.
    fn min_level(&self) -> OptLevel;

    /// Whether this pass is `size`-only (`-Os`). Default false. `minify-rename` overrides to true.
    fn os_only(&self) -> bool {
        false
    }

    /// Default gate: eligible if `level.tier() >= min_level.tier()` (and, for size-only
    /// passes, size mode). Passes with extra conditions (ts-typeof) override this.
    fn should_run(&self, cfg: &PassConfig) -> bool {
        if self.os_only() {
            return cfg.size;
        }
        cfg.level.tier() >= self.min_level().tier()
    }

    /// Run the pass once over the whole program.
    ///
    /// Implementations typically build a `Traverse` visitor and call
    /// [`run_traverse`] (which wraps `oxc_traverse::traverse_mut`, threading the
    /// `Scoping` by value and assigning the returned one back through `scoping`).
    ///
    /// `allocator` is the SAME arena the `Program` lives in (required so new nodes
    /// share the lifetime `'a`).
    fn run<'a>(
        &mut self,
        program: &mut Program<'a>,
        scoping: &mut Scoping,
        allocator: &'a Allocator,
        cfg: &PassConfig,
    ) -> PassResult;
}

/// Helper that bridges the `Scoping`-by-value contract of `oxc_traverse::traverse_mut`
/// to the `&mut Scoping` the `Pass::run` API exposes.
///
/// Moves the `Scoping` out of `*scoping`, runs the traversal, and assigns the returned
/// `Scoping` back. New pass authors should call this from their `run` impl.
pub fn run_traverse<'a, State, Tr>(
    visitor: &mut Tr,
    allocator: &'a Allocator,
    program: &mut Program<'a>,
    scoping: &mut Scoping,
    state: State,
) where
    Tr: oxc_traverse::Traverse<'a, State>,
{
    let taken = std::mem::take(scoping);
    let returned = oxc_traverse::traverse_mut(visitor, allocator, program, taken, state);
    *scoping = returned;
}

/// Per-pass overrides parsed from `--enable-<pass>` / `--disable-<pass>`.
/// Disable wins over enable.
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    pub enabled: HashSet<String>,
    pub disabled: HashSet<String>,
}

/// Constructors for every registered pass, in canonical run order:
///   ts-typeof-elimination, constant-folding, peephole-algebraic,
///   dead-code-elimination, minify-rename.
/// (typeof first to expose constants; fold; simplify; then DCE removes now-dead
///  branches; rename last.)
type PassCtor = fn() -> Box<dyn Pass>;

fn pass_registry() -> Vec<PassCtor> {
    vec![
        || Box::new(ts_typeof::TsTypeofElimination) as Box<dyn Pass>,
        || Box::new(propagation::Propagation) as Box<dyn Pass>,
        || Box::new(pure_eval::PureEval) as Box<dyn Pass>,
        || Box::new(constant_fold::ConstantFolding) as Box<dyn Pass>,
        || Box::new(peephole::PeepholeAlgebraic) as Box<dyn Pass>,
        || Box::new(control_flow::ControlFlowSimplification) as Box<dyn Pass>,
        || Box::new(cse_gvn::CseGvn) as Box<dyn Pass>,
        || Box::new(object_construct::ObjectConstruction) as Box<dyn Pass>,
        || Box::new(array_construct::ArrayConstruction) as Box<dyn Pass>,
        || Box::new(inlining::Inlining) as Box<dyn Pass>,
        || Box::new(param_scalarize::ParamScalarization) as Box<dyn Pass>,
        || Box::new(dead_store::DeadStoreElimination) as Box<dyn Pass>,
        || Box::new(dead_param::DeadParamElimination) as Box<dyn Pass>,
        || Box::new(dce::DeadCodeElimination) as Box<dyn Pass>,
        || Box::new(licm::Licm) as Box<dyn Pass>,
        || Box::new(rename::MinifyRename) as Box<dyn Pass>,
    ]
}

/// Select the ordered, gated pass list for the chosen config + overrides.
///
/// `-O0` short-circuits to an EMPTY list -> driver runs pure passthrough.
pub fn select(cfg: &PassConfig, overrides: &Overrides) -> Vec<Box<dyn Pass>> {
    if cfg.level == OptLevel::O0 {
        return vec![];
    }
    let mut out: Vec<Box<dyn Pass>> = vec![];
    for ctor in pass_registry() {
        let p = ctor();
        let id = p.name();
        if overrides.disabled.contains(id) {
            continue; // disable wins
        }
        let gated_in = p.should_run(cfg) || overrides.enabled.contains(id);
        if gated_in {
            out.push(p);
        }
    }
    out
}
