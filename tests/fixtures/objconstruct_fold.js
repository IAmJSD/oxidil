// Positive case: a freshly-declared object built up by a run of own-property
// stores folds into one literal. Effectful (non-throwing) RHS is allowed here
// because the fold site is inside a function with no enclosing `try`.
"use strict";
function build(g) {
  var x = {};
  x.a = 1;
  x.b = g();
  x["c-d"] = 3;
  x[0] = "zero";
  return x;
}
const o = build(() => 2);
console.log(JSON.stringify(o));
console.log(o["c-d"], o[0]);
