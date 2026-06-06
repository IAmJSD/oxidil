# oxidil

A small **JavaScript / TypeScript optimizing compiler** written in Rust on top of
the [oxc](https://github.com/oxc-project/oxc) crate ecosystem (pinned to
`v0.134.0`). It parses JS/TS, optionally strips TypeScript types, runs a set of
**sound** optimization passes gated by a GCC-flavored `-O` level, and emits
optimized JavaScript plus a v3 source map.

Everything runs **in-process**: the produced binary never shells out to
`node`/`terser`/`esbuild`/`swc` or any other external program at runtime. All
work is done via linked Rust crates.

## Build

```sh
cargo build --release
# binary at ./target/release/oxidil
```

Toolchain: Rust 1.96.0 / cargo 1.96.0. The oxc crates are pinned to `0.134.0`
(which requires Rust >= 1.94).

## CLI usage

```
oxidil <INPUT> --out <FILE> [--out-map <FILE>] [--source-map <FILE>]
        [-O<level>] [--ts-typeof]
        [--enable <ID>]... [--disable <ID>]...
```

| Flag | Meaning |
|------|---------|
| `<INPUT>` | Input file. `SourceType` is inferred from the extension: `.js .jsx .mjs .cjs .ts .tsx`. |
| `--out <FILE>` | **Required.** Where optimized JS is written. |
| `--out-map <FILE>` | Where the output source map is written. If omitted, no map file is written and no `sourceMappingURL` comment is appended (codegen still produces a valid map internally). |
| `--source-map <FILE>` | Path to an INPUT source map (the map for `<INPUT>`). When present, the final `--out-map` is **composed** so it points all the way back to the ORIGINAL authored sources. |
| `--ts-typeof` | Orthogonal flag enabling the `ts-typeof-elimination` pass (effective only at level >= 1; never at `-O0`). |
| `-O0` *(also `-0`, `--O0`)* | No optimization (debug/baseline). |
| `-O1` *(also `-1`, `--O1`, bare `-O`)* | Constant folding + peephole/algebraic. |
| `-O2` *(also `-2`, `--O2`)* | **Default.** `-O1` + dead-code elimination at full fixpoint. |
| `-O3` *(also `-3`, `--O3`)* | `-O2` with a higher fixpoint cap (more aggressive). |
| `-Os` *(also `-s`, `--Os`, `-Oz`)* | `-O2` + identifier minify/rename + compact codegen (size). |

Optimization levels use GCC-canonical spelling (`-O2`, `-Os`, bare `-O` = `-O1`); the
short `-2`/`-s` and long `--O2`/`--Os` aliases are equivalent.
| `--enable <ID>` | Force-include a pass by id even if the level would gate it off. Repeatable. |
| `--disable <ID>` | Force-exclude a pass by id. Repeatable. **Disable wins over enable.** |

### Optimization levels and which passes each enables

The driver selects which passes run, purely from the chosen `-O` level (passes
are gated centrally in the registry by a level/threshold, not by editing pass
code). `--ts-typeof` is orthogonal and layers on at any level >= 1.

| Level | Passes enabled | Fixpoint cap |
|-------|----------------|--------------|
| `-O0` | *(none — parse → type-strip if TS → codegen passthrough)* | 0 |
| `-O1` | propagation, pure-eval, constant-folding, peephole-algebraic, control-flow-simplification | 1 (light) |
| `-O2` | `-O1` + cse-gvn, inlining, dead-store-elimination, dead-param-elimination, dead-code-elimination | 8 (full fixpoint) |
| `-O3` | `-O2` passes + licm | 16 (more iterations) |
| `-Os` | `-O2` passes + minify-rename + compact codegen | 8 |
| `--ts-typeof` | adds ts-typeof-elimination at any level >= 1 | — |

Per-pass level assignment encoded in the registry (gated centrally by
`min_level()` / `os_only()`):

| Pass id | Min level | Notes |
|---------|-----------|-------|
| `ts-typeof-elimination` | O1 + `--ts-typeof` flag | orthogonal flag |
| `propagation` | O1 | const-prop + copy-prop |
| `pure-eval` | O1 | pure built-in / global folding |
| `constant-folding` | O1 | |
| `peephole-algebraic` | O1 | |
| `control-flow-simplification` | O1 | if/else, blocks, switch-on-const |
| `cse-gvn` | O2 | local value numbering |
| `inlining` | O2 | single-use variable inlining |
| `dead-store-elimination` | O2 | |
| `dead-param-elimination` | O2 | trailing unused params |
| `dead-code-elimination` | O2 | |
| `licm` | O3 | loop-invariant code motion |
| `minify-rename` | `-Os` only | |

Run order is the table order above (typeof first to expose constants; propagate /
fold / simplify; then CSE / inline; then dead-store / dead-param / DCE; LICM;
rename last).

## Optimization passes

Pass ids (for `--enable` / `--disable`), in canonical run order. Every pass is
**conservative**: when it cannot *prove* a transform observationally equivalent
for all inputs it skips the transform. Each pass also bails entirely on programs
containing `with` or a direct `eval(...)` (scope/reference data is then
unreliable).

1. **ts-typeof-elimination** (`ts-typeof-elimination`) — see [the section
   below](#the---ts-typeof-feature).
2. **propagation** (`propagation`, O1) — constant propagation + copy propagation
   for non-reassigned, non-closed-over locals. **Module root scope is now in
   scope:** a top-level `const`/`let` in an **ES module** is module-private (not a
   property of the global object, invisible to sibling scripts), so its reads may
   be propagated (even when exported — only the read sites change; the live export
   slot is untouched). In a **script** the top level is shared global-lexical /
   global-object state, so root-scope bindings are still refused. **Soundness
   boundary:** only propagates when the declaration **dominates** every use — `var`
   is never propagated (hoisted-`undefined` reads), and for `let`/`const` the
   declarator must sit at the top level of its scope (not under any branch/loop)
   and every read must be textually after the initializer (so a Temporal-Dead-Zone
   read still throws). For **copy-prop** the alias's initializer must itself be
   throwless: every free identifier it reads must be provably already-initialized
   at the declarator (a `var`/function, or a lexical binding declared textually
   before it), so propagating `const a = later; const later = 5;` cannot move (and
   thereby delete) the alias's TDZ `ReferenceError`. Only finite-number / string /
   bool / null literals are synthesized; copy-prop re-checks shadowing per use site.
3. **pure-eval** (`pure-eval`, O1) — evaluates pure host built-ins
   (`Math.*`, `String.fromCharCode`, `Number()/String()/Boolean()`,
   `"…".length`, etc.) on constant arguments via `oxc_ecmascript`, trusting a
   name to be the genuine global only when it resolves to no local symbol.
   **Soundness boundary (GRANULAR monkey-patch guard):** instead of bailing the
   whole pass on any global write, the pass computes the exact set of global
   **names** that are monkey-patched and suppresses folding only for those; every
   other built-in still folds (so `parseInt("42")` folds even when `Math.floor` is
   patched). A name is poisoned when:
   - it is the target of an assignment / update (`Math = …`, `Math.floor = …`,
     including inside **destructuring** patterns `[Math.floor] = …` and `for`-`of`/
     `for`-`in` heads), **or**
   - it is the **leaf** of a write through a global-object alias
     (`globalThis`/`window`/`self`/`global`/`frames`): `globalThis.Math = 1`,
     `globalThis["Math"] = 1`, and `globalThis.Math.floor = f` all poison `Math`
     (not the alias root) — this is robust to constant-folding rewriting a computed
     key like `"Ma"+"th"` to `"Math"`, **or**
   - it flows as the target object into a known mutator —
     `Object.defineProperty`/`defineProperties`/`assign`/`setPrototypeOf`,
     `Reflect.defineProperty`/`set`/`setPrototypeOf`,
     `x.__defineGetter__`/`__defineSetter__`.

   When the patched name cannot be determined statically — a computed **non-literal**
   key on a global-object alias (`globalThis[k] = v`), or a mutator applied to the
   global object itself (`Object.defineProperty(globalThis, …)`) — the pass falls
   back to the conservative **whole-pass bail** (treat every global as patched).
   Aliasing a global into a local first (`const m = Math; m.floor = f`) is a known
   limitation that static analysis here does not catch. Never synthesizes
   NaN/Infinity/BigInt/undefined.
4. **constant-folding** (`constant-folding`, O1) — folds provably-constant,
   **side-effect-free** expressions to literals via `oxc_ecmascript`'s
   `ConstantEvaluation`. Consults `may_have_side_effects` before replacing, so
   `(f(), 5)` is **not** folded to `5` (the call to `f()` is preserved).
5. **peephole-algebraic** (`peephole-algebraic`, O1) — sound algebraic / boolean
   simplifications: `!(!!x) → !x`, constant `?:` and `&&`/`||`/`??`
   short-circuit, and numeric identities (`x*1`, `x/1`, `x-0`) gated on a
   known-Number operand. Omits `x+0` and `x*0` (`-0`/`NaN` hazards) and all
   bitwise rewrites.
6. **control-flow-simplification** (`control-flow-simplification`, O1) —
   else-after-return hoisting, `if`→ternary for matching simple arms, safe block
   flattening / empty removal, and switch-on-constant folding. **Soundness
   boundary for switch folding:** refuses if any DROPPED case (before the matched
   case, or any case when nothing matches and there is no default) hoists a
   `var`/`function`/`class`, and refuses if the kept region contains a
   `break`/`continue` (checked **recursively** through blocks/if/try, but not
   into nested loops/switches that capture their own jumps) that would retarget
   once the switch is removed. Preserves `let`/`const` TDZ and block scoping.
7. **cse-gvn** (`cse-gvn`, O2) — common-subexpression elimination via local value
   numbering: a repeated, pure, non-trivial expression over never-mutated locals
   is computed once into a fresh `const` temp. **Soundness boundary:** coercion
   (ToPrimitive) is itself an observable side effect, and `may_have_side_effects`
   does **not** flag relational/equality/arithmetic operators over identifier
   operands, so the eligible operator set is restricted to ops that run **no
   ToPrimitive hook and cannot throw on operand type**: strict equality
   (`===`/`!==`), logical (`&&`/`||`/`??`), unary `!`/`typeof`/`void`, **plus**
   arithmetic / bitwise (`+ - * / % ** & | ^ << >> >>>`) **when every operand is a
   provably-finite-`Number`** (a never-mutated `const`/`let` whose initializer is,
   transitively, a finite numeric literal or such an op over provably-number
   operands). Numbers are already primitive, so no coercion runs and `number op
   number` never throws — collapsing N evaluations to one changes neither value nor
   observable coercions. An operand that could be an object with a `valueOf`/
   `toString` hook is excluded. Member/call/`new` excluded.
8. **inlining** (`inlining`, O2) — single-use variable inlining: a
   single-assignment, single-use, non-closed-over local with a **pure**
   initializer has its initializer moved to the use. Module-root non-exported
   `const`/`let` are now inline-eligible (module-private; an exported root binding
   keeps its slot, and a script's root is never touched). **Soundness boundary:**
   the declaration must **dominate** the use (use textually after the init); if the
   declarator and use are in the **same lexical scope** the control-flow-nesting
   bail is relaxed (block statements run top-to-bottom and cannot be entered
   mid-way) — **except inside a `switch`**, where `case` labels jump forward into
   the shared block scope past an earlier `const`'s initializer, so a switch-nested
   declarator is never relaxed (the cross-case read is a TDZ access that must
   throw). The initializer must also be **throwless**: every free identifier it
   reads must be provably already-initialized at the declarator (so moving
   `const a = later; const later = 5;` cannot delete the TDZ throw). If the
   initializer reads any free local, the use must be in the declarator's scope so
   no intervening block can re-resolve (shadow) that identifier. Never inlines a
   function (this/arguments/recursion hazards).
9. **dead-store-elimination** (`dead-store-elimination`, O2) — removes
   assignments whose value is never read (and no-op `x = x`). **Soundness
   boundary:** never touches a `const`/`let`/`class` (block-scoped) target — the
   write itself can throw (const TypeError, TDZ ReferenceError) — and never a
   function **parameter** (writing it is observable via a mapped `arguments`
   object in sloppy mode). Only simple `=` identifier targets; member stores
   (setters/proxies) excluded.
10. **dead-param-elimination** (`dead-param-elimination`, O2) — drops trailing
    unused parameters of a local function whose every use is a direct call, and
    the matching trailing args at each call site. **Soundness boundary:** a
    dropped argument must be side-effect-free **and** must not read a lexical
    (`let`/`const`/`class`) binding that could be in its TDZ at the call site
    (evaluating it throws — dropping it would delete the throw). Spreads, default
    params, rest params, destructuring, generators, and `arguments` users are
    excluded.
11. **dead-code-elimination** (`dead-code-elimination`, O2) — drops empty
    statements, truncates code after an unconditional terminator, collapses
    `if(<const>)`, and removes unused local `let`/`const`/`function` bindings via
    `oxc_semantic` reference counts. **Module-root non-exported bindings are now
    removable** (module-private); a **script**'s root scope and any **exported**
    module binding are still never removed. **Soundness boundary:** an unused
    declarator is removed only when its initializer is absent, or is
    side-effect-free **and throwless** w.r.t. identifier reads — every free
    identifier read must be provably already-initialized at the declarator (a
    `var`/function, or a lexical binding declared textually before it). This keeps
    `const unused = later; const later = 5;` (a sibling TDZ read) and
    `const unused = notDefined;` (an undeclared-global read) from having their
    `ReferenceError` deleted.
12. **licm** (`licm`, O3) — loop-invariant code motion: a pure, non-trivial
    invariant expression in a loop body is hoisted to a `const` temp before the
    loop. **Soundness boundary:** same non-coercing operator restriction as
    cse-gvn (including the **provably-finite-`Number`** operand broadening for the
    coercing arithmetic / bitwise ops), plus a TDZ-hoist guard — a block-scoped
    operand whose declaration is at/after the loop is rejected (reading it at the
    pre-loop site would throw when the loop runs zero times). Only never-mutated
    local / literal operands.
13. **minify-rename** (`minify-rename`, `-Os` only) — short identifier names via
    `oxc_mangler`'s scope analysis. Bails the whole pass on `with`/direct-`eval`.

## The `--ts-typeof` feature

`--ts-typeof` learns, for some local bindings, a statically-known runtime
`typeof` result, and folds `typeof x === "T"` (and `!==`/`==`/`!=`, plus
`switch (typeof x)`) into booleans so DCE can prune dead branches.

**Soundness:** TypeScript annotations are *erased*, not enforced at runtime, so
the pass **never** folds based on a parameter (or otherwise externally-supplied)
annotation — a value of a different runtime type routinely reaches an annotated
parameter via `any`, type assertions, or untyped JS callers. The only fact
source is a **`const` binding to a simple identifier whose initializer's runtime
`typeof` can be pinned directly from the initializer expression** (e.g.
`const n = 42` → number, `const s = "x"` → string). Everything else poisons the
name: parameters, `let`/`var`, reassignments (including destructuring/member
assignment targets), `x++`, catch params, destructuring patterns, function/class
declaration names, and imports. Facts are keyed by name and a name is foldable
only if every binding/use agrees.

So `function f(x: number) { if (typeof x === "string") ... }` is left untouched,
while `const n = 42; typeof n === "number"` folds to `true`.

## Source-map composition

- With `--out-map` only: emits a valid v3 map from generated positions to the
  input file.
- With `--out-map` **and** `--source-map <input.map>`: composes the codegen map
  with the input map so generated positions map back to the ORIGINAL authored
  sources. Token *name* ids are translated into the output map's own `names`
  table (looked up by string and re-registered), so the `names` array is
  correctly populated and every mapping's name index is in bounds — original
  identifier names (e.g. renamed-back parameter names) are preserved.

## Sample commands

```sh
# Baseline (no opt), still emits a valid map:
oxidil sample.js --out out.js --out-map out.js.map -0

# Constant folding + peephole only:
oxidil sample.js --out out.js -1

# Default: O1 + DCE at full fixpoint:
oxidil sample.js --out out.js -2

# Aggressive:
oxidil sample.js --out out.js -3

# Size: minify + rename + compact output:
oxidil sample.js --out out.min.js -s

# TypeScript with typeof elimination + composed source map:
oxidil examples/sample.ts --out out.js --out-map out.js.map \
        --source-map in.map --O2 --ts-typeof

# Override the level for one pass:
oxidil sample.js --out out.js -2 --disable dead-code-elimination
oxidil sample.js --out out.js -1 --enable dead-code-elimination
```

## Testing

```sh
# Unit + integration tests (the integration suite drives the real binary and,
# when `node` is on PATH, runs a differential check of -O1..-Os against -O0).
cargo test --release

# Differential corpus harness: compiles every program in difftest/corpus at
# each level (plus the --ts-typeof variants for .ts files), runs each through
# node, and reports any divergence from the -O0 baseline. Exits non-zero on a
# divergence. Needs `node` on PATH; build the release binary first (or point
# OXIDIL_BIN at another build).
cargo build --release
bash difftest/run.sh
```

CI (`.github/workflows/test.yml`) runs four jobs on every push — build & test,
the differential corpus harness, `rustfmt --check`, and `clippy -D warnings` —
on Rust 1.96.0 and Node 24, with the cargo registry and `target/` cached across
runs.

## Limitations

Every pass favors correctness over completeness: it misses opportunities it
cannot prove safe rather than risk a behavior change. Notable conservative gaps:

- `ts-typeof-elimination` only acts on `const` bindings with a directly
  pinnable literal/expression initializer. Parameter/exported/imported/`let`
  annotations are never folded. `object`-typed values are never pinned.
- **propagation** only fires for `let`/`const` whose declarator is at the top
  level of its scope (not under a branch/loop) and precedes all uses in source
  order. `var` is never propagated, and a binding whose declarator is nested in
  any control-flow construct is skipped — so some genuinely-dominating uses are
  missed. Cross-fixpoint cascades are relied upon for chains.
- **pure-eval** bails for the ENTIRE program if any global identifier or global
  property is written anywhere — one `Math.foo = …` disables all built-in
  folding program-wide (a deliberately blunt, sound over-approximation).
- **cse-gvn** / **licm** only handle expressions built from strict-equality,
  logical, and `!`/`typeof`/`void` operators over never-mutated locals/literals;
  arithmetic, relational, and bitwise operators are excluded (their operand
  coercion is an observable side effect). LICM additionally rejects block-scoped
  operands declared at/after the loop (TDZ), which a precise reaching-definition
  analysis could sometimes admit.
- **inlining** only inlines single-use variables (never functions), and requires
  the use to be in the declarator's own scope when the initializer reads a free
  local (a stricter rule than strictly necessary, but cheaply sound against
  block shadowing).
- **dead-store-elimination** never touches block-scoped (`let`/`const`/`class`)
  or parameter targets, so only `var`/assignable-local dead stores are removed.
- **dead-param-elimination** keeps a trailing argument that reads any lexical
  binding (even one already initialized at the call site) to avoid deleting a
  possible TDZ throw — a conservative over-approximation.
- **control-flow-simplification**'s switch-on-constant fold keeps the switch
  whenever the matched region contains a `break`/`continue` (it does not yet
  rewrite a clean trailing `break` into a fall-through-free block), so many
  break-terminated switches are left intact.
- **Module vs. script root scope** (gated on the post-parse `SourceType`): in an
  **ES module** (`.mjs`/`.mts`, or a `.js`/`.ts` that actually uses
  `import`/`export`) the top-level `const`/`let` are module-private — not
  properties of the global object and not visible to sibling scripts — so
  propagating, folding, inlining, and DCE-removing them is observationally
  invisible outside the module and is now **enabled**. An **exported** binding
  keeps its live slot (its reads may be propagated, but the binding itself is never
  removed/moved). In a **script** (`.js`/`.cjs` with no module syntax) the top
  level is shared global-lexical / global-object state observable by sibling
  scripts, so all root-scope optimization there stays **refused**.
- Root-scope removal/inlining/propagation additionally requires the moved/deleted
  initializer to be **throwless** w.r.t. identifier reads (no TDZ / undeclared-
  global `ReferenceError` may be deleted by relocating or dropping the read).
- The same-scope inlining broadening does **not** apply across `switch` case
  labels (forward case-jumps bypass earlier `const` initializers).
- Every symbol-touching pass bails entirely on `with`/direct-`eval` programs.
- BigInt and `undefined` constants are not synthesized by any folder.

## License

[MIT](LICENSE) © 2026 Astrid Gealer
