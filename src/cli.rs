//! CLI definition (clap derive).
//!
//! Synopsis:
//!   oxidil <INPUT> --out <FILE> [--out-map <FILE>] [--source-map <FILE>]
//!           [-O<level>] [--ts-typeof]
//!           [--enable <ID>]... [--disable <ID>]...

use std::path::PathBuf;

use clap::Parser;

use oxidil::level::OptLevel;
use oxidil::pass::Overrides;

#[derive(Parser, Debug)]
#[command(
    name = "oxidil",
    about = "A Rust JS/TS optimizing compiler (oxc front-end)",
    version
)]
pub struct Cli {
    /// Input file: .js/.jsx/.ts/.tsx/.mjs/.cjs. SourceType is inferred from the extension.
    pub input: PathBuf,

    /// Where optimized JS is written (required).
    #[arg(long)]
    pub out: PathBuf,

    /// Where the output source map is written. If omitted, no map file is written
    /// (codegen still produces a valid map internally).
    #[arg(long)]
    pub out_map: Option<PathBuf>,

    /// Path to an INPUT source map (the map for <INPUT>). When present, the final
    /// --out-map is COMPOSED so it points to the ORIGINAL sources.
    #[arg(long)]
    pub source_map: Option<PathBuf>,

    /// Orthogonal flag: enable ts-typeof-elimination (effective only at level >= 1).
    #[arg(long)]
    pub ts_typeof: bool,

    // ---- optimization level: 5 mutually-exclusive bool flags (default -O2) ----
    /// -O0: no optimization (parse -> type-strip if TS -> codegen passthrough).
    #[arg(short = '0', long = "O0", group = "optlevel")]
    pub o0: bool,
    /// -O1: constant-folding + peephole/algebraic.
    #[arg(short = '1', long = "O1", group = "optlevel")]
    pub o1: bool,
    /// -O2 (default): O1 + dead-code-elimination at full fixpoint.
    #[arg(short = '2', long = "O2", group = "optlevel")]
    pub o2: bool,
    /// -O3: O2 with higher fixpoint cap / more aggressive settings.
    #[arg(short = '3', long = "O3", group = "optlevel")]
    pub o3: bool,
    /// -Os: O2 + minify-rename + compact codegen (size).
    #[arg(short = 's', long = "Os", group = "optlevel")]
    pub os: bool,

    /// Force-include a pass by id even if the level would gate it off. Repeatable.
    #[arg(long = "enable", value_name = "ID")]
    pub enable: Vec<String>,

    /// Force-exclude a pass by id. Repeatable. Disable wins over enable.
    #[arg(long = "disable", value_name = "ID")]
    pub disable: Vec<String>,
}

impl Cli {
    /// Map the 5 bool flags to an `OptLevel`; absence => default (`O2`).
    pub fn opt_level(&self) -> OptLevel {
        if self.o0 {
            OptLevel::O0
        } else if self.o1 {
            OptLevel::O1
        } else if self.o3 {
            OptLevel::O3
        } else if self.os {
            OptLevel::Os
        } else {
            // o2 explicitly, or nothing given.
            OptLevel::O2
        }
    }

    pub fn overrides(&self) -> Overrides {
        Overrides {
            enabled: self.enable.iter().cloned().collect(),
            disabled: self.disable.iter().cloned().collect(),
        }
    }
}
