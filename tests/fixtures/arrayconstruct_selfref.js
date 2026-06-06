// Soundness boundary: a store RHS that reads the target binding (`x[0] = x`) must
// not fold — `var x = [x]` reads the hoisted `undefined`, not the live array.
function f() {
  var x = [];
  x[0] = x;
  x[1] = 2;
  return x[0] === x && x[1] === 2;
}
console.log(f());
