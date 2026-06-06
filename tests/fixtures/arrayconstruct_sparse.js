// Soundness boundary: a gap makes the array sparse. Only the contiguous prefix
// folds (`var x = [1]; x[2] = 3;`), preserving the hole at index 1 — folding the
// whole run into a dense `[1, 3]` would move element 3 to index 1 and drop the
// hole. We print enough to observe holes, length, and indices.
function f() {
  var x = [];
  x[0] = 1;
  x[2] = 3;
  return [JSON.stringify(x), x.length, 1 in x, 2 in x];
}
console.log(JSON.stringify(f()));
