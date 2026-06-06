//! The compile pipeline. Owns the single arena and threads AST + allocator +
//! semantic info through: parse -> (type-strip if TS) -> semantic ->
//! pass fixpoint -> codegen -> source-map compose.
//!
//! This module is **pure**: it operates on in-memory strings and performs no
//! file I/O, so it links cleanly into both the native CLI binary and the
//! `wasm32` build. The binary (`src/main.rs`) is responsible for reading the
//! input, building [`CompileOptions`], and writing the outputs.

use std::path::Path;
use std::rc::Rc;

use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions, CodegenReturn};
use oxc_parser::{ParseOptions, Parser};
use oxc_span::SourceType;

use crate::error::{CompileError, Result};
use crate::level::OptLevel;
use crate::pass::{self, Overrides, PassConfig};
use crate::semantic_util::rebuild_scoping;
use crate::{sourcemap, ts_strip};

/// Everything the compile core needs, independent of where the bytes came from.
#[derive(Debug, Clone)]
pub struct CompileOptions {
    /// Optimization level (default `O2`).
    pub level: OptLevel,
    /// Enable the `ts-typeof-elimination` pass (effective only at level >= 1).
    pub ts_typeof: bool,
    /// Per-pass force enable/disable. Disable wins over enable.
    pub overrides: Overrides,
    /// Logical name of the input. Drives `SourceType` inference (by extension)
    /// and the `sources` entry of the produced source map. For the CLI this is
    /// the input path; for the WASM API it is a caller-supplied filename.
    pub filename: String,
    /// Whether to produce an output source map at all.
    pub source_map: bool,
    /// JSON of an INPUT source map (the map for `filename`). When present and
    /// `source_map` is true, the output map is COMPOSED so it points back to the
    /// ORIGINAL authored sources.
    pub input_source_map: Option<String>,
}

impl Default for CompileOptions {
    fn default() -> Self {
        CompileOptions {
            level: OptLevel::O2,
            ts_typeof: false,
            overrides: Overrides::default(),
            filename: "input.js".to_string(),
            source_map: true,
            input_source_map: None,
        }
    }
}

/// Result of a compile: optimized JS plus an optional v3 source map (JSON).
///
/// `code` does NOT carry a trailing `//# sourceMappingURL=` comment; appending
/// one (with whatever map filename the caller chose) is the caller's job.
#[derive(Debug, Clone)]
pub struct CompileOutput {
    pub code: String,
    pub map: Option<String>,
}

/// Compile a source string end-to-end. Pure: no file I/O.
pub fn compile(source_text: &str, opts: &CompileOptions) -> Result<CompileOutput> {
    let level = opts.level;
    let overrides = &opts.overrides;

    // SourceType is inferred from the filename extension (.js/.jsx/.ts/.tsx/.mjs/.cjs).
    let source_type = SourceType::from_path(Path::new(&opts.filename)).unwrap_or_default();

    // Arena.
    let allocator = Allocator::default();

    // PARSE.
    // `preserve_parens: false` removes `ParenthesizedExpression` wrappers so the
    // constant-evaluation machinery (which does not recurse through them) can see
    // foldable operands like `(2 as number) + 3` / `(1 + 2) * 3`.
    let parse_opts = ParseOptions {
        preserve_parens: false,
        ..ParseOptions::default()
    };
    let ret = Parser::new(&allocator, source_text, source_type)
        .with_options(parse_opts)
        .parse();
    if ret.panicked || !ret.errors.is_empty() {
        return Err(CompileError::ParseErrors(ret.errors));
    }
    let mut program = ret.program;

    // HARVEST TS typeof-facts from the still-typed AST, BEFORE stripping erases
    // the annotations. Only meaningful for TS input with --ts-typeof; cheap no-op
    // otherwise (plain JS has no annotations => empty facts).
    let typeof_facts = if opts.ts_typeof && source_type.is_typescript() {
        Rc::new(pass::ts_typeof::collect(&program))
    } else {
        Rc::new(pass::TypeofFacts::default())
    };

    // Authoritative module-vs-script predicate, read from the POST-PARSE
    // `program.source_type` (Unambiguous `.js/.ts` are now resolved to
    // Module/Script by whether they contain import/export).
    let is_module = crate::semantic_util::is_es_module(&program);

    // Collect exported top-level binding symbols (empty for scripts). Needs a
    // scoping snapshot to resolve `export { local }` specifiers.
    let exported: std::collections::HashSet<oxc_syntax::symbol::SymbolId> = if is_module {
        let scoping = rebuild_scoping(&program);
        crate::semantic_util::collect_exported_symbols(&program, &scoping)
    } else {
        std::collections::HashSet::new()
    };

    let cfg = PassConfig {
        level,
        ts_typeof: opts.ts_typeof,
        size: level.size(),
        typeof_facts,
        is_module,
        exported: Rc::new(exported),
    };

    // TYPE-STRIP for TS input (ALL levels incl. O0).
    if source_type.is_typescript() {
        let scoping = rebuild_scoping(&program);
        ts_strip::strip(&mut program, &allocator, scoping);
    }

    // PASS SELECTION (empty for O0).
    let mut passes = pass::select(&cfg, overrides);

    // Final scoping carried out of the fixpoint loop. In size mode (`-Os`) the
    // minify-rename pass mangles symbol names INTO this scoping; codegen then
    // prints those renamed names via `with_scoping`.
    let mut final_scoping: Option<oxc_semantic::Scoping> = None;

    // FIXPOINT LOOP (skipped if no passes).
    if !passes.is_empty() {
        let cap = level.fixpoint_cap();
        let mut scoping = rebuild_scoping(&program);
        let mut iter = 0u32;
        loop {
            if level.tier() >= OptLevel::O2.tier() && iter > 0 {
                scoping = rebuild_scoping(&program);
            }
            let mut changed_any = false;
            for p in passes.iter_mut() {
                let r = p.run(&mut program, &mut scoping, &allocator, &cfg);
                changed_any |= r.changed;
            }
            iter += 1;
            if !changed_any || iter >= cap {
                break;
            }
        }
        if cfg.size {
            final_scoping = Some(scoping);
        }
    }

    // CODEGEN. Setting source_map_path => `map` is Some (valid map even at O0).
    let codegen_opts = CodegenOptions {
        source_map_path: if opts.source_map {
            Some(Path::new(&opts.filename).to_path_buf())
        } else {
            None
        },
        minify: cfg.size,
        single_quote: cfg.size,
        ..CodegenOptions::default()
    };
    let CodegenReturn { code, map, .. } = Codegen::new()
        .with_options(codegen_opts)
        .with_scoping(final_scoping)
        .build(&program);

    // SOURCE MAP COMPOSE.
    let final_map = match map {
        Some(cg_map) => {
            let in_map = match &opts.input_source_map {
                Some(json) => Some(
                    oxc_sourcemap::SourceMap::from_json_string(json)
                        .map_err(|e| CompileError::SourceMapError(e.to_string()))?
                        .into_owned(),
                ),
                None => None,
            };
            Some(sourcemap::compose(cg_map.into(), in_map).to_json_string())
        }
        None => None,
    };

    Ok(CompileOutput {
        code,
        map: final_map,
    })
}
