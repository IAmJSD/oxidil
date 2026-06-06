//! Pass #5: identifier minify / rename (`-Os` only).
//!
//! Scope-safe identifier mangling built on `oxc_mangler` + `oxc_semantic`.
//!
//! ## How it works (and why it is sound)
//! The mangler does NOT rewrite the AST. It computes short, collision-free names
//! per *symbol* (using esbuild-style slot/liveness analysis over the scope tree)
//! and stores them in the `Scoping`'s symbol-name table via `set_symbol_name`.
//! `oxc_codegen`, when handed that `Scoping` through `with_scoping`, then prints
//! the renamed name for every binding/reference whose AST node carries a resolved
//! `symbol_id`/`reference_id`, while every node keeps its ORIGINAL `Span`. So the
//! source map still maps each renamed identifier back to its original name and
//! position — renaming is purely a codegen-time name substitution.
//!
//! Soundness comes from the mangler's scope analysis, which we rely on for:
//!   - shadowing is respected (slot liveness is a subtree of the scope tree),
//!   - never renames across scopes incorrectly (graph-coloring over live scopes),
//!   - globals / unresolved references are reserved and never reused as targets,
//!   - exports are preserved (top-level module exports are not renamed),
//!   - `arguments` and reserved keywords are never produced or renamed.
//!
//! For non-module scripts we leave `top_level: None`, so the mangler's default
//! (`false` for plain scripts) keeps top-level / global-ish names untouched —
//! the conservative choice when we cannot prove a top-level name is local.
//!
//! WHAT THE MANGLER DOES NOT GUARD (so we guard it here): `with` and direct
//! `eval`. Inside a `with (obj) { ... }` body identifier resolution is dynamic
//! against `obj`'s (possibly unknown) properties, so renaming a local that is
//! referenced there can flip which value wins (binding vs. with-object property).
//! Direct `eval("...")` can reference any in-scope binding by its ORIGINAL name
//! from a string the mangler never inspects. In either case renaming is unsound,
//! so if the program contains ANY `with` statement or direct `eval(...)` call we
//! skip the entire rename pass (conservative, matching the DCE bail-out).
//!
//! ## Pipeline contract
//! This pass mutates the `Scoping` it is given (it REPLACES `*scoping` with the
//! mangler's freshly-built scoping, which is consistent with the program's
//! re-populated `symbol_id`/`reference_id` cells). The driver, in size mode, hands
//! that final `Scoping` to codegen via `with_scoping`. Compact output (whitespace /
//! semicolon stripping, single quotes) is handled by the driver's `minify` codegen
//! option, also gated on size mode.
//!
//! Source-map note: a renamed identifier keeps its original `Span`, so REFERENCE
//! (use) sites map back to the original name via the source map's `names` table;
//! binding sites map back to their original source position (the original name is
//! recorded for usages, not for the binding site itself).

use oxc_allocator::Allocator;
use oxc_ast::ast::Program;
use oxc_mangler::{MangleOptions, Mangler};
use oxc_semantic::Scoping;

use crate::level::OptLevel;
use crate::pass::{Pass, PassConfig, PassResult};
use crate::semantic_util::program_has_with_or_eval;

#[derive(Default)]
pub struct MinifyRename;

impl Pass for MinifyRename {
    fn name(&self) -> &'static str {
        "minify-rename"
    }

    fn min_level(&self) -> OptLevel {
        // Gated by `os_only()` rather than tier, but report Os for diagnostics.
        OptLevel::Os
    }

    fn os_only(&self) -> bool {
        true
    }

    fn run<'a>(
        &mut self,
        program: &mut Program<'a>,
        scoping: &mut Scoping,
        _allocator: &'a Allocator,
        _cfg: &PassConfig,
    ) -> PassResult {
        // SOUNDNESS BAIL-OUT: `with` makes identifier resolution dynamic, and
        // direct `eval(...)` can reference any in-scope binding by its ORIGINAL
        // name from a string the mangler never sees. The mangler does not guard
        // either, so renaming would be unsound. Skip the whole pass if present.
        if program_has_with_or_eval(program) {
            return PassResult::UNCHANGED;
        }

        // Snapshot the current symbol names so we can detect whether mangling
        // actually changed anything (drives the fixpoint's `changed` flag and
        // keeps the loop terminating: a second run produces identical names).
        let before: Vec<String> = scoping.symbol_names().map(str::to_owned).collect();

        // Run the mangler. It builds its own `Semantic` from `program` (which
        // re-populates the program's `symbol_id`/`reference_id` cells), assigns
        // short names into that semantic's scoping, and returns it. We adopt that
        // scoping as our authoritative one so it stays consistent with the AST
        // cells codegen will read.
        //
        // `top_level: None` -> mangler default (rename top-level only for modules;
        // never for plain scripts, preserving global-ish names). `debug: false`
        // -> base54 short names (`a`, `b`, ...). `keep_names` default false.
        let options = MangleOptions {
            top_level: None,
            keep_names: oxc_mangler::MangleOptionsKeepNames::default(),
            debug: false,
        };
        let result = Mangler::new().with_options(options).build(program);
        *scoping = result.scoping;

        // Did any symbol name change?
        let changed = scoping
            .symbol_names()
            .zip(before.iter())
            .any(|(now, was)| now != was.as_str())
            || scoping.symbols_len() != before.len();

        if changed {
            PassResult::CHANGED
        } else {
            PassResult::UNCHANGED
        }
    }
}
