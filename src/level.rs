//! Optimization level system. GCC-flavored, adapted for JS.
//!
//! Level gating does NOT rely on the derived enum order (so that `Os` is not
//! treated as "greater" than `O3`). Instead each level maps to an explicit
//! numeric *tier* used for `>=` comparisons, plus orthogonal flags (`size`).

/// The user-selected optimization level.
///
/// Derived `Ord` yields `O0 < O1 < O2 < O3 < Os`, but we deliberately route
/// all gating through [`OptLevel::tier`] rather than the raw ordering.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Default)]
pub enum OptLevel {
    O0,
    O1,
    #[default]
    O2,
    O3,
    Os,
}

impl OptLevel {
    /// Numeric tier used for `level.tier() >= min_level.tier()` gating.
    /// `Os` is an O2-tier level that additionally sets the `size` flag.
    pub fn tier(self) -> u8 {
        match self {
            OptLevel::O0 => 0,
            OptLevel::O1 => 1,
            OptLevel::O2 => 2,
            OptLevel::O3 => 3,
            OptLevel::Os => 2,
        }
    }

    /// Whether this level optimizes for size (`-Os`): enables rename + compact codegen.
    pub fn size(self) -> bool {
        matches!(self, OptLevel::Os)
    }

    /// Fixpoint iteration cap for the driver loop.
    pub fn fixpoint_cap(self) -> u32 {
        match self {
            OptLevel::O0 => 0,
            OptLevel::O1 => 1,
            OptLevel::O2 => 8,
            OptLevel::O3 => 16,
            OptLevel::Os => 8,
        }
    }
}
