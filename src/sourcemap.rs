//! Source map composition.
//!
//! Two maps are in play:
//!   - `cg_map`: produced by codegen. Maps GENERATED positions -> positions in the
//!     INPUT file we parsed.
//!   - `in_map` (optional, from `--source-map`): maps INPUT positions -> ORIGINAL
//!     author sources.
//!
//! We want the final map to point GENERATED -> ORIGINAL, so we walk one hop through
//! `in_map` for every codegen token.

use std::collections::HashMap;

use oxc_sourcemap::{SourceMap, SourceMapBuilder};

/// Compose the codegen map with an optional input map.
///
/// When `in_map` is `None`, returns `cg_map` unchanged. Otherwise produces a new
/// map mapping generated positions all the way back to the original sources.
pub fn compose<'a>(cg_map: SourceMap<'a>, in_map: Option<SourceMap<'a>>) -> SourceMap<'a> {
    let Some(in_map) = in_map else {
        return cg_map;
    };

    let in_lut = in_map.generate_lookup_table();
    let mut out = SourceMapBuilder::default();

    // Map input-map source ids -> output builder source ids (registered lazily).
    let mut src_id_of: HashMap<u32, u32> = HashMap::new();
    // Resolve a name id from EITHER the codegen map or the input map into a name
    // id valid for the OUTPUT builder. A name id is an index into a specific map's
    // own `names` table; copying it verbatim corrupts the output map (the indices
    // would point into the output's empty/foreign `names` array). We instead look
    // up the actual name STRING and (re-)register it in the output builder.
    let resolve_name = |out: &mut SourceMapBuilder,
                        cg_name_id: Option<u32>,
                        orig_name_id: Option<u32>|
     -> Option<u32> {
        let name: Option<String> = cg_name_id
            .and_then(|n| cg_map.get_name(n))
            .or_else(|| orig_name_id.and_then(|n| in_map.get_name(n)))
            .map(|s| s.to_string());
        name.map(|s| out.add_name(&s))
    };
    // Pseudo source id used for codegen tokens that have no original mapping.
    const INPUT_PSEUDO: u32 = u32::MAX;

    for cg_tok in cg_map.get_tokens() {
        // The codegen token's "src" position is a position in the INPUT file.
        let in_line = cg_tok.get_src_line();
        let in_col = cg_tok.get_src_col();

        if let Some(orig) = in_map.lookup_token(&in_lut, in_line, in_col) {
            if let Some(orig_sid) = orig.get_source_id() {
                let oid = *src_id_of.entry(orig_sid).or_insert_with(|| {
                    let name = in_map
                        .get_source(orig_sid)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("source{orig_sid}"));
                    let content = in_map
                        .get_source_content(orig_sid)
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    out.add_source_and_content(&name, &content)
                });
                let name_id = resolve_name(&mut out, cg_tok.get_name_id(), orig.get_name_id());
                out.add_token(
                    cg_tok.get_dst_line(),
                    cg_tok.get_dst_col(),
                    orig.get_src_line(),
                    orig.get_src_col(),
                    Some(oid),
                    name_id,
                );
                continue;
            }
        }

        // No original mapping: fall back to mapping into the input file directly,
        // reusing whatever sources the codegen map already declared.
        let id = *src_id_of.entry(INPUT_PSEUDO).or_insert_with(|| {
            let name = cg_tok
                .get_source_id()
                .and_then(|sid| cg_map.get_source(sid))
                .map(|s| s.to_string())
                .unwrap_or_else(|| "input".to_string());
            let content = cg_tok
                .get_source_id()
                .and_then(|sid| cg_map.get_source_content(sid))
                .map(|s| s.to_string())
                .unwrap_or_default();
            out.add_source_and_content(&name, &content)
        });
        let name_id = resolve_name(&mut out, cg_tok.get_name_id(), None);
        out.add_token(
            cg_tok.get_dst_line(),
            cg_tok.get_dst_col(),
            in_line,
            in_col,
            Some(id),
            name_id,
        );
    }

    out.into_sourcemap()
}
