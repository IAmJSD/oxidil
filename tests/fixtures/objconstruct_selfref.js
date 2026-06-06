// Soundness boundary: a store RHS that reads the target binding (`x.self = x`)
// must not be folded — `var x = {self: x}` reads the hoisted `undefined`, not the
// live object. The pass leaves the run unfolded, so `x.self === x` stays true.
function f() {
  var x = {};
  x.self = x;
  x.n = 5;
  return x.self === x && x.n === 5;
}
console.log(f());
