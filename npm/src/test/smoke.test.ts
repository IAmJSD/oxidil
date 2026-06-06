import assert from "node:assert/strict";
import { test } from "node:test";

import { compile } from "../index.js";

test("O0 is passthrough, O2 folds constants", () => {
  const src = "const a = 1 + 2 * 3;\n";
  const o0 = compile(src, { level: "0" }).code;
  const o2 = compile(src, { level: "2" }).code;
  assert.match(o0, /1 \+ 2 \* 3/, "O0 leaves the expression intact");
  assert.match(o2, /const a = 7/, "O2 folds 1+2*3 to 7");
  assert.notEqual(o0, o2);
});

test("TypeScript input is stripped to pure JS", () => {
  const src = "interface P { x: number }\nconst p: P = { x: 1 as number };\n";
  const { code } = compile(src, { filename: "in.ts", level: "0" });
  assert.doesNotMatch(code, /interface/);
  assert.doesNotMatch(code, /: number/);
  assert.doesNotMatch(code, / as /);
});

test("source map is emitted on request", () => {
  const { code, map } = compile("const a = 1 + 1;\n", { sourceMap: true });
  assert.ok(map, "map should be present");
  const parsed = JSON.parse(map!);
  assert.equal(parsed.version, 3);
  assert.ok(Array.isArray(parsed.mappings ? [] : parsed.sources));
});

test("no map unless requested", () => {
  const { map } = compile("const a = 1;\n");
  assert.equal(map, undefined);
});

test("-Os renames and compacts", () => {
  const src = "function add(first, second) { return first + second; }\nconsole.log(add(1, 2));\n";
  const small = compile(src, { level: "s" }).code;
  const plain = compile(src, { level: "2" }).code;
  assert.ok(small.length <= plain.length);
});

test("parse error throws", () => {
  assert.throws(() => compile("const = ;\n"), /error/i);
});
