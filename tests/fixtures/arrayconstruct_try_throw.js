// Soundness boundary: inside a `try`, only the leading literal store folds; a
// possibly-throwing store stays separate so the catch still observes the
// partially-built array [1].
function f() {
  var x = [];
  try {
    x[0] = 1;
    x[1] = (() => { throw new Error("boom"); })();
    x[2] = 3;
  } catch (e) {
    return JSON.stringify(x);
  }
  return JSON.stringify(x);
}
console.log(f());
