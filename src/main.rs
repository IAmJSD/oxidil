//! oxidil: a Rust JS/TS optimizing compiler with an oxc front-end.
//!
//! Thin entry: parse CLI, run the driver, map errors to exit codes.

mod cli;
mod driver;
mod error;
mod level;
mod pass;
mod semantic_util;
mod sourcemap;
mod ts_strip;

use clap::Parser;

use crate::cli::Cli;

fn main() {
    let cli = Cli::parse_from(normalize_opt_flags(std::env::args().collect()));
    match driver::run(&cli) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(e.exit_code());
        }
    }
}

/// Accept GCC-canonical `-O<level>` tokens (`-O0`/`-O1`/`-O2`/`-O3`/`-Os`/`-Oz`,
/// and bare `-O` == `-O1`) by rewriting them to oxidil's underlying level flags
/// before clap parsing. The pre-existing `-0/-1/-2/-3/-s` and `--O0..--Os`
/// spellings keep working unchanged.
fn normalize_opt_flags(args: Vec<String>) -> Vec<String> {
    args.into_iter()
        .map(|a| match a.as_str() {
            "-O" | "-O1" => "--O1".to_string(),
            "-O0" => "--O0".to_string(),
            "-O2" => "--O2".to_string(),
            "-O3" => "--O3".to_string(),
            "-Os" | "-Oz" => "--Os".to_string(),
            _ => a,
        })
        .collect()
}
