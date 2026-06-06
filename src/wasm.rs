//! `wasm-bindgen` bindings exposing the compile core to JavaScript.
//!
//! Built only with `--features wasm` (see Cargo.toml). The generated glue is
//! consumed by the `oxidil` npm package's TypeScript wrapper.

use wasm_bindgen::prelude::*;

use crate::driver::{self, CompileOptions};
use crate::level::OptLevel;
use crate::pass::Overrides;

/// Result of a successful compile, surfaced to JS as an object with `code` and
/// (optional) `map` string getters.
#[wasm_bindgen]
pub struct CompileResult {
    code: String,
    map: Option<String>,
}

#[wasm_bindgen]
impl CompileResult {
    /// Optimized JavaScript source.
    #[wasm_bindgen(getter)]
    pub fn code(&self) -> String {
        self.code.clone()
    }

    /// v3 source map as a JSON string, or `undefined` when maps were disabled.
    #[wasm_bindgen(getter)]
    pub fn map(&self) -> Option<String> {
        self.map.clone()
    }
}

fn parse_level(level: &str) -> OptLevel {
    match level {
        "0" | "O0" => OptLevel::O0,
        "1" | "O1" => OptLevel::O1,
        "3" | "O3" => OptLevel::O3,
        "s" | "z" | "Os" | "Oz" => OptLevel::Os,
        // "2" / "O2" / anything else => default.
        _ => OptLevel::O2,
    }
}

/// Compile a JS/TS source string. Mirrors the native CLI semantics.
///
/// - `source`: the input source text.
/// - `filename`: logical name; `SourceType` is inferred from its extension and
///   it becomes the `sources` entry of the output map.
/// - `level`: optimization level — `"0".."3"` or `"s"`/`"z"` (default `"2"`).
/// - `ts_typeof`: enable the `ts-typeof-elimination` pass.
/// - `enable` / `disable`: pass ids to force on/off (disable wins).
/// - `source_map`: produce an output map.
/// - `input_source_map`: JSON of an input map to compose against.
#[wasm_bindgen(js_name = compile)]
#[allow(clippy::too_many_arguments)]
pub fn compile(
    source: &str,
    filename: &str,
    level: &str,
    ts_typeof: bool,
    enable: Vec<String>,
    disable: Vec<String>,
    source_map: bool,
    input_source_map: Option<String>,
) -> std::result::Result<CompileResult, JsError> {
    let opts = CompileOptions {
        level: parse_level(level),
        ts_typeof,
        overrides: Overrides {
            enabled: enable.into_iter().collect(),
            disabled: disable.into_iter().collect(),
        },
        filename: filename.to_string(),
        source_map,
        input_source_map,
    };

    match driver::compile(source, &opts) {
        Ok(out) => Ok(CompileResult {
            code: out.code,
            map: out.map,
        }),
        Err(e) => Err(JsError::new(&e.to_string())),
    }
}
