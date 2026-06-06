#!/usr/bin/env node
/**
 * oxidil CLI — a thin Node front-end over the wasm compile core.
 *
 * Mirrors the native Rust binary's interface:
 *   oxidil <INPUT> --out <FILE> [--out-map <FILE>] [--source-map <FILE>]
 *           [-O<level>] [--ts-typeof]
 *           [--enable <ID>]... [--disable <ID>]...
 *
 * Exit codes match the native binary: 0 ok, 1 parse error, 2 IO/other,
 * 64 usage error.
 */

import { readFileSync, writeFileSync } from "node:fs";
import { basename } from "node:path";

import { compile, type OptLevel } from "./index.js";
// `version.ts` is generated from Cargo.toml (the source of truth) by the
// Makefile and is gitignored. Run `make generate` (or `make`) before building.
import { VERSION } from "./version.js";

const USAGE = `oxidil ${VERSION} — a JS/TS optimizing compiler (oxc front-end, wasm)

USAGE:
    oxidil <INPUT> --out <FILE> [OPTIONS]

ARGS:
    <INPUT>                Input file (.js/.jsx/.mjs/.cjs/.ts/.tsx).

OPTIONS:
    --out <FILE>           Where optimized JS is written. (required)
    --out-map <FILE>       Where the output source map is written.
    --source-map <FILE>    Input source map for <INPUT>; output map is composed
                           back to the original sources.
    --ts-typeof            Enable ts-typeof-elimination (level >= 1 only).
    -O0|-O1|-O2|-O3|-Os    Optimization level (default -O2). -O == -O1, -Oz == -Os.
                           Aliases: -0/-1/-2/-3/-s and --O0..--Os.
    --enable <ID>          Force-include a pass by id (repeatable).
    --disable <ID>         Force-exclude a pass by id (repeatable). Disable wins.
    -h, --help             Print help.
    -V, --version          Print version.`;

class UsageError extends Error {}

interface ParsedArgs {
  input?: string;
  out?: string;
  outMap?: string;
  sourceMap?: string;
  tsTypeof: boolean;
  level: OptLevel;
  enable: string[];
  disable: string[];
}

/** Map an `-O`/`-N` token to a level, or `undefined` if it isn't one. */
function levelOfToken(tok: string): OptLevel | undefined {
  switch (tok) {
    case "-O":
    case "-O1":
    case "--O1":
    case "-1":
      return "1";
    case "-O0":
    case "--O0":
    case "-0":
      return "0";
    case "-O2":
    case "--O2":
    case "-2":
      return "2";
    case "-O3":
    case "--O3":
    case "-3":
      return "3";
    case "-Os":
    case "--Os":
    case "-s":
      return "s";
    case "-Oz":
      return "z";
    default:
      return undefined;
  }
}

function needValue(argv: string[], i: number, flag: string): string {
  const v = argv[i + 1];
  if (v === undefined || v.startsWith("-")) {
    throw new UsageError(`option '${flag}' requires a value`);
  }
  return v;
}

function parseArgs(argv: string[]): ParsedArgs {
  const parsed: ParsedArgs = {
    tsTypeof: false,
    level: "2",
    enable: [],
    disable: [],
  };

  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];

    const lvl = levelOfToken(a);
    if (lvl !== undefined) {
      parsed.level = lvl;
      continue;
    }

    // Support `--flag=value` spellings as well as `--flag value`.
    const eq = a.startsWith("--") ? a.indexOf("=") : -1;
    const flag = eq >= 0 ? a.slice(0, eq) : a;
    const inlineVal = eq >= 0 ? a.slice(eq + 1) : undefined;
    const value = (f: string) => inlineVal ?? needValue(argv, i, f);
    const consume = () => {
      if (inlineVal === undefined) i++;
    };

    switch (flag) {
      case "-h":
      case "--help":
        console.log(USAGE);
        process.exit(0);
      // eslint-disable-next-line no-fallthrough
      case "-V":
      case "--version":
        console.log(VERSION);
        process.exit(0);
      // eslint-disable-next-line no-fallthrough
      case "--out":
        parsed.out = value(flag);
        consume();
        break;
      case "--out-map":
        parsed.outMap = value(flag);
        consume();
        break;
      case "--source-map":
        parsed.sourceMap = value(flag);
        consume();
        break;
      case "--ts-typeof":
        parsed.tsTypeof = true;
        break;
      case "--enable":
        parsed.enable.push(value(flag));
        consume();
        break;
      case "--disable":
        parsed.disable.push(value(flag));
        consume();
        break;
      default:
        if (a.startsWith("-")) {
          throw new UsageError(`unexpected option '${a}'`);
        }
        if (parsed.input !== undefined) {
          throw new UsageError(`unexpected argument '${a}' (input already set)`);
        }
        parsed.input = a;
    }
  }

  return parsed;
}

function main(): void {
  let args: ParsedArgs;
  try {
    args = parseArgs(process.argv.slice(2));
  } catch (e) {
    process.stderr.write(`${(e as Error).message}\n\n${USAGE}\n`);
    process.exit(64);
  }

  if (args.input === undefined) {
    process.stderr.write(`missing required <INPUT>\n\n${USAGE}\n`);
    process.exit(64);
  }
  if (args.out === undefined) {
    process.stderr.write(`missing required --out <FILE>\n\n${USAGE}\n`);
    process.exit(64);
  }

  let source: string;
  let inputSourceMap: string | undefined;
  try {
    source = readFileSync(args.input, "utf8");
    if (args.sourceMap !== undefined) {
      inputSourceMap = readFileSync(args.sourceMap, "utf8");
    }
  } catch (e) {
    process.stderr.write(`IO error: ${(e as Error).message}\n`);
    process.exit(2);
  }

  let code: string;
  let map: string | undefined;
  try {
    const result = compile(source, {
      filename: args.input,
      level: args.level,
      tsTypeof: args.tsTypeof,
      enable: args.enable,
      disable: args.disable,
      sourceMap: true,
      inputSourceMap,
    });
    code = result.code;
    map = result.map;
  } catch (e) {
    // The wasm core throws a single Error whose message is the formatted
    // compiler diagnostic (parse errors, etc.).
    process.stderr.write(`${(e as Error).message}\n`);
    process.exit(1);
  }

  try {
    if (args.outMap !== undefined && map !== undefined) {
      writeFileSync(args.outMap, map);
      if (!code.endsWith("\n")) code += "\n";
      code += `//# sourceMappingURL=${basename(args.outMap)}\n`;
    }
    writeFileSync(args.out, code);
  } catch (e) {
    process.stderr.write(`IO error: ${(e as Error).message}\n`);
    process.exit(2);
  }
}

main();
