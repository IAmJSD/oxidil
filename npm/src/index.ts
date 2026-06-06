/**
 * oxidil — programmatic API.
 *
 * Thin, typed wrapper over the wasm-bindgen glue generated from the Rust crate.
 * The heavy lifting (parse, type-strip, optimization passes, codegen) all runs
 * inside the WebAssembly module; this layer only marshals options and results.
 */

import { compile as wasmCompile } from "../wasm/oxidil.js";

/**
 * Optimization level, GCC-flavored:
 *  - `"0"` no optimization (parse -> type-strip if TS -> passthrough)
 *  - `"1"` constant folding + peephole/algebraic
 *  - `"2"` (default) `1` + dead-code elimination at full fixpoint
 *  - `"3"` `2` with a higher fixpoint cap (more aggressive)
 *  - `"s"` / `"z"` `2` + identifier minify/rename + compact codegen (size)
 *
 * Numbers (`0`–`3`) are accepted too and coerced to the string form.
 */
export type OptLevel = "0" | "1" | "2" | "3" | "s" | "z" | 0 | 1 | 2 | 3;

export interface CompileOptions {
  /**
   * Logical input name. The `SourceType` is inferred from its extension
   * (`.js .jsx .mjs .cjs .ts .tsx`) and it becomes the `sources` entry of the
   * output map. Defaults to `"input.js"`.
   */
  filename?: string;
  /** Optimization level. Defaults to `"2"`. */
  level?: OptLevel;
  /** Enable the `ts-typeof-elimination` pass (effective only at level >= 1). */
  tsTypeof?: boolean;
  /** Force-include passes by id, even if the level would gate them off. */
  enable?: string[];
  /** Force-exclude passes by id. Disable wins over enable. */
  disable?: string[];
  /** Produce an output source map. Defaults to `false`. */
  sourceMap?: boolean;
  /**
   * JSON of an INPUT source map (the map for `filename`). When provided together
   * with `sourceMap`, the output map is composed so it points back to the
   * ORIGINAL authored sources.
   */
  inputSourceMap?: string;
}

export interface CompileResult {
  /** Optimized JavaScript source. */
  code: string;
  /** v3 source map (JSON string), present only when `sourceMap` was enabled. */
  map?: string;
}

/**
 * Compile (optimize) a JavaScript or TypeScript source string.
 *
 * @example
 * ```ts
 * import { compile } from "oxidil";
 * const { code } = compile("const a = 1 + 2 * 3;", { level: "2" });
 * // code === "const a = 7;\n"
 * ```
 */
export function compile(source: string, options: CompileOptions = {}): CompileResult {
  const filename = options.filename ?? "input.js";
  const level = String(options.level ?? "2");
  const sourceMap = options.sourceMap ?? false;

  const result = wasmCompile(
    source,
    filename,
    level,
    options.tsTypeof ?? false,
    options.enable ?? [],
    options.disable ?? [],
    sourceMap,
    options.inputSourceMap ?? undefined,
  );

  try {
    const out: CompileResult = { code: result.code };
    const map = result.map;
    if (map !== undefined) {
      out.map = map;
    }
    return out;
  } finally {
    // Release the wasm-owned struct; getters above already copied the strings.
    result.free();
  }
}
