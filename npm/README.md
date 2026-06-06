# oxidil

A **JavaScript / TypeScript optimizing compiler** — the Rust [oxidil](https://github.com/astridgealer/oxidil)
crate (built on the [oxc](https://github.com/oxc-project/oxc) ecosystem) compiled
to WebAssembly and wrapped in a typed Node API and CLI.

It parses JS/TS, optionally strips TypeScript types, runs a set of **sound**
optimization passes gated by a GCC-flavored `-O` level, and emits optimized
JavaScript plus a v3 source map. Everything runs in-process inside the WASM
module — no `node`/`terser`/`esbuild`/`swc` is shelled out to.

## Install

```sh
npm install oxidil
```

Requires Node.js >= 18. The WebAssembly module is bundled — there is no native
build step at install time.

## API

```ts
import { compile } from "oxidil";

const { code, map } = compile("const a = 1 + 2 * 3;\nconsole.log(a);\n", {
  level: "2",
  sourceMap: true,
});

console.log(code); // "const a = 7;\nconsole.log(a);\n"
```

### `compile(source, options?) => { code, map? }`

| Option | Type | Default | Meaning |
|--------|------|---------|---------|
| `filename` | `string` | `"input.js"` | Source type is inferred from the extension (`.js .jsx .mjs .cjs .ts .tsx`); also used as the `sources` entry of the map. |
| `level` | `"0"\|"1"\|"2"\|"3"\|"s"\|"z"` (or `0`–`3`) | `"2"` | Optimization level (see below). |
| `tsTypeof` | `boolean` | `false` | Enable the `ts-typeof-elimination` pass (effective only at level >= 1). |
| `enable` | `string[]` | `[]` | Force-include passes by id even if the level gates them off. |
| `disable` | `string[]` | `[]` | Force-exclude passes by id. Disable wins over enable. |
| `sourceMap` | `boolean` | `false` | Produce an output v3 source map (returned as `map`, a JSON string). |
| `inputSourceMap` | `string` | — | JSON of an input map; the output map is composed back to the original sources. |

## CLI

```
oxidil <INPUT> --out <FILE> [--out-map <FILE>] [--source-map <FILE>]
        [-O<level>] [--ts-typeof]
        [--enable <ID>]... [--disable <ID>]...
```

```sh
npx oxidil app.ts --out app.js --out-map app.js.map -O2
```

| Flag | Meaning |
|------|---------|
| `<INPUT>` | Input file. Source type inferred from extension. |
| `--out <FILE>` | **Required.** Where optimized JS is written. |
| `--out-map <FILE>` | Where the output source map is written (and a `//# sourceMappingURL=` comment is appended). |
| `--source-map <FILE>` | Input source map for `<INPUT>`; the output map is composed back to the original sources. |
| `--ts-typeof` | Enable `ts-typeof-elimination`. |
| `-O0`/`-O1`/`-O2`/`-O3`/`-Os` | Optimization level (default `-O2`). `-O` == `-O1`, `-Oz` == `-Os`. Aliases `-0`..`-3`/`-s` and `--O0`..`--Os` also work. |
| `--enable <ID>` / `--disable <ID>` | Force a pass on/off (repeatable; disable wins). |

Exit codes: `0` ok, `1` parse error, `2` IO error, `64` usage error.

## Optimization levels

| Level | Passes |
|-------|--------|
| `-O0` | None (parse → type-strip if TS → passthrough). |
| `-O1` | Constant folding + peephole/algebraic. |
| `-O2` *(default)* | `-O1` + dead-code elimination at full fixpoint. |
| `-O3` | `-O2` with a higher fixpoint cap (more aggressive). |
| `-Os` / `-Oz` | `-O2` + identifier minify/rename + compact codegen (size). |

## Building from source

The published package ships prebuilt WASM. To rebuild it you need the Rust
toolchain plus [`wasm-pack`](https://rustwasm.github.io/wasm-pack/). The version
lives in the crate's `Cargo.toml` (the source of truth); `npm/package.json` and
`npm/src/version.ts` are generated from it and are gitignored — so build from the
repo root with `make`:

```sh
make npm     # generates the version files, then runs wasm-pack + tsc
```

Or, if the generated files already exist (`make generate`):

```sh
npm run build        # build:wasm (wasm-pack) + build:ts (tsc)
npm test
```

## License

MIT
