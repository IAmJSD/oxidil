// Soundness boundary: inside a `try`, a store whose RHS may throw must NOT be
// folded into the literal — folding would leave `x` unassigned (undefined) when
// the initializer throws partway, while the original keeps the partially-built
// object visible in the catch. Only the leading literal store (`x.a = 1`) is safe
// to fold; the throwing store stays separate, so the catch still observes {a:1}.
function f() {
  var x = {};
  try {
    x.a = 1;
    x.b = (() => { throw new Error("boom"); })();
    x.c = 3;
  } catch (e) {
    return JSON.stringify(x);
  }
  return JSON.stringify(x);
}
console.log(f());
