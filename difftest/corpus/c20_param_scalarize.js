// param-scalarization for nested (non-global) script functions, incl. recursion
// and side-effect-ordered values.
function run() {
  const log = [];
  function f(opts) { return opts.a * opts.b + (opts.c || 0); }
  const r1 = f({ a: 2, b: (log.push("b"), 3) });
  const r2 = f({ a: 4, b: 5, c: 6 });
  return [r1, r2, log.join(",")];
}
console.log(JSON.stringify(run()));

function outer() {
  // the recursive call is itself a rewritten call site
  function fact(opts) { return opts.n <= 1 ? 1 : opts.n * fact({ n: opts.n - 1 }); }
  return fact({ n: 5 });
}
console.log(outer());

// escape inside a script function: not split
function holder() {
  function keep(opts) { globalThis.__k = opts; return opts.v; }
  return keep({ v: 9 });
}
console.log(holder(), globalThis.__k.v);
