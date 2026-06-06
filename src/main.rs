//! oxidil CLI: parse CLI, read input, run the compile core, write outputs,
//! map errors to exit codes. All file I/O lives here; the optimization core
//! ([`oxidil::compile`]) is pure and shared with the WASM build.

mod cli;

use std::fs;
use std::path::Path;

use clap::Parser;

use cli::Cli;
use oxidil::driver::CompileOptions;
use oxidil::error::Result;

fn main() {
    let cli = Cli::parse_from(normalize_opt_flags(std::env::args().collect()));
    match run(&cli) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(e.exit_code());
        }
    }
}

/// Read input, build options, compile, and write the requested outputs.
fn run(cli: &Cli) -> Result<()> {
    let source_text = fs::read_to_string(&cli.input)?;

    let input_source_map = match &cli.source_map {
        Some(p) => Some(fs::read_to_string(p)?),
        None => None,
    };

    let opts = CompileOptions {
        level: cli.opt_level(),
        ts_typeof: cli.ts_typeof,
        overrides: cli.overrides(),
        filename: cli.input.to_string_lossy().into_owned(),
        source_map: true,
        input_source_map,
    };

    let out = oxidil::driver::compile(&source_text, &opts)?;
    let mut code = out.code;

    // Persist the map + append sourceMappingURL only when --out-map is requested.
    if let (Some(out_map_path), Some(map_json)) = (&cli.out_map, &out.map) {
        fs::write(out_map_path, map_json)?;
        let basename = basename(out_map_path);
        if !code.ends_with('\n') {
            code.push('\n');
        }
        code.push_str(&format!("//# sourceMappingURL={basename}\n"));
    }

    fs::write(&cli.out, code)?;
    Ok(())
}

fn basename(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
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
