//! The compile pipeline. Owns the single arena and threads AST + allocator +
//! semantic info through: read -> parse -> (type-strip if TS) -> semantic ->
//! pass fixpoint -> codegen -> source-map compose -> write.

use std::fs;
use std::path::Path;
use std::rc::Rc;

use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions, CodegenReturn};
use oxc_parser::{ParseOptions, Parser};
use oxc_span::SourceType;

use crate::cli::Cli;
use crate::error::{CompileError, Result};
use crate::level::OptLevel;
use crate::pass::{self, PassConfig};
use crate::semantic_util::rebuild_scoping;
use crate::{sourcemap, ts_strip};

/// Run the full compile per the parsed CLI options.
pub fn run(cli: &Cli) -> Result<()> {
    let level = cli.opt_level();
    let overrides = cli.overrides();

    // 1. Read input. Own the source so it outlives the arena borrow.
    let source_text: String = fs::read_to_string(&cli.input)?;
    let source_type = SourceType::from_path(&cli.input).unwrap_or_default();

    // 2. Arena.
    let allocator = Allocator::default();

    // 3. PARSE.
    // `preserve_parens: false` removes `ParenthesizedExpression` wrappers so the
    // constant-evaluation machinery (which does not recurse through them) can see
    // foldable operands like `(2 as number) + 3` / `(1 + 2) * 3`.
    let parse_opts = ParseOptions {
        preserve_parens: false,
        ..ParseOptions::default()
    };
    let ret = Parser::new(&allocator, &source_text, source_type)
        .with_options(parse_opts)
        .parse();
    if ret.panicked || !ret.errors.is_empty() {
        return Err(CompileError::ParseErrors(ret.errors));
    }
    let mut program = ret.program;

    // 3b. HARVEST TS typeof-facts from the still-typed AST, BEFORE stripping erases
    // the annotations. Only meaningful for TS input with --ts-typeof; cheap no-op
    // otherwise (plain JS has no annotations => empty facts).
    let typeof_facts = if cli.ts_typeof && source_type.is_typescript() {
        Rc::new(pass::ts_typeof::collect(&program))
    } else {
        Rc::new(pass::TypeofFacts::default())
    };

    // Authoritative module-vs-script predicate, read from the POST-PARSE
    // `program.source_type` (Unambiguous `.js/.ts` are now resolved to
    // Module/Script by whether they contain import/export). Do NOT use the
    // pre-parse `source_type` variable above (still Unambiguous for `.js/.ts`).
    let is_module = crate::semantic_util::is_es_module(&program);

    // Collect exported top-level binding symbols (empty for scripts). Needs a
    // scoping snapshot to resolve `export { local }` specifiers. Cheap for the
    // common no-export case (walks only `program.body`).
    let exported: std::collections::HashSet<oxc_syntax::symbol::SymbolId> = if is_module {
        let scoping = rebuild_scoping(&program);
        crate::semantic_util::collect_exported_symbols(&program, &scoping)
    } else {
        std::collections::HashSet::new()
    };

    let cfg = PassConfig {
        level,
        ts_typeof: cli.ts_typeof,
        size: level.size(),
        typeof_facts,
        is_module,
        exported: Rc::new(exported),
    };

    // 4. TYPE-STRIP for TS input (ALL levels incl. O0).
    // traverse_mut requires populated scope ids, so build scoping from this program first.
    if source_type.is_typescript() {
        let scoping = rebuild_scoping(&program);
        ts_strip::strip(&mut program, &allocator, scoping);
    }

    // 5/6. PASS SELECTION (empty for O0).
    let mut passes = pass::select(&cfg, &overrides);

    // Final scoping carried out of the fixpoint loop. In size mode (`-Os`) the
    // minify-rename pass mangles symbol names INTO this scoping; codegen then
    // prints those renamed names via `with_scoping` (positions/spans unchanged,
    // so the source map still maps back to original names). `None` => codegen
    // prints the AST's own identifier names (no rename, e.g. at -O1/-O2/-O3).
    let mut final_scoping: Option<oxc_semantic::Scoping> = None;

    // 7. FIXPOINT LOOP (skipped if no passes).
    if !passes.is_empty() {
        let cap = level.fixpoint_cap();
        // Build scoping once up front.
        let mut scoping = rebuild_scoping(&program);
        let mut iter = 0u32;
        loop {
            // At O2+, rebuild fresh reference counts each iteration so liveness-
            // sensitive passes (DCE) see up-to-date info.
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
        // Keep the final scoping so size-mode codegen can render mangled names.
        if cfg.size {
            final_scoping = Some(scoping);
        }
    }

    // 8. CODEGEN. Setting source_map_path => `map` is Some (valid map even at O0).
    let codegen_opts = CodegenOptions {
        source_map_path: Some(cli.input.clone()),
        minify: cfg.size,
        single_quote: cfg.size,
        ..CodegenOptions::default()
    };
    let CodegenReturn { mut code, map, .. } = Codegen::new()
        .with_options(codegen_opts)
        .with_scoping(final_scoping)
        .build(&program);

    // 9. SOURCE MAP COMPOSE.
    let final_map = match map {
        Some(cg_map) => {
            let in_map = match &cli.source_map {
                Some(p) => {
                    let json = fs::read_to_string(p)?;
                    // `from_json_string` borrows `json`; `into_owned` lifts it to
                    // `'static` so the parsed map can outlive this local string.
                    Some(
                        oxc_sourcemap::SourceMap::from_json_string(&json)
                            .map_err(|e| CompileError::SourceMapError(e.to_string()))?
                            .into_owned(),
                    )
                }
                None => None,
            };
            // Codegen now yields an `OwnedSourceMap`; convert to a `SourceMap`.
            Some(sourcemap::compose(cg_map.into(), in_map))
        }
        None => None,
    };

    // 9b. Persist the map + append sourceMappingURL only when --out-map is requested.
    if let (Some(out_map_path), Some(m)) = (&cli.out_map, &final_map) {
        let json = m.to_json_string();
        fs::write(out_map_path, json)?;
        let basename = basename(out_map_path);
        if !code.ends_with('\n') {
            code.push('\n');
        }
        code.push_str(&format!("//# sourceMappingURL={basename}\n"));
    }

    // 10. Write code.
    fs::write(&cli.out, code)?;
    Ok(())
}

fn basename(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}
