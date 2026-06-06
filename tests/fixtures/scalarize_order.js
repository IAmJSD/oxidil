// Boundary: two sites order their side-effecting values differently, so there is
// no single param order that preserves evaluation order — must bail.
function build() {
  const order = [];
  function f(opts) { return opts.a - opts.b; }
  const r1 = f({ a: (order.push("a1"), 10), b: (order.push("b1"), 1) });
  const r2 = f({ b: (order.push("b2"), 2), a: (order.push("a2"), 20) });
  return [r1, r2, order.join(",")];
}
console.log(JSON.stringify(build()));
