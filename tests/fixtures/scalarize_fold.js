// Positive: a non-escaping nested function with an options-object param is split
// into scalar params, and call sites pass values positionally — including a
// value-mutated key, a side-effecting value, and missing keys (filled `void 0`).
function build() {
  const log = [];
  function f(opts) {
    opts.a = opts.a + 1;
    return opts.a + (opts.b || 0) + (opts.c || 0);
  }
  const r1 = f({ a: 1, b: (log.push("b"), 10) });
  const r2 = f({ a: 2, c: 100 });
  return [r1, r2, log.join(",")];
}
console.log(JSON.stringify(build()));
