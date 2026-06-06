// Positive case: a dense ascending run of indexed stores folds into an array
// literal, including into a non-empty base and with an effectful (non-throwing)
// RHS (allowed because the fold site is inside a function with no enclosing try).
"use strict";
function build(g) {
  var x = [];
  x[0] = 1;
  x[1] = g();
  x[2] = "z";
  return x;
}
function extend() {
  var y = [10, 20];
  y[2] = 30;
  y[3] = 40;
  return y;
}
console.log(JSON.stringify(build(() => 2)), build(() => 2).length);
console.log(JSON.stringify(extend()), extend().length);
