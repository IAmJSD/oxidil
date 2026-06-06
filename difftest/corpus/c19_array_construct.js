// array-construction folding: an ascending, dense, contiguous run of indexed
// stores becomes one array literal — plus the soundness boundaries.

// basic fold: literal + effectful (non-throwing) RHS
function build(g) {
  var x = [];
  x[0] = 1;
  x[1] = g();
  x[2] = "z";
  return x;
}
console.log(JSON.stringify(build(() => 2)), build(() => 2).length);

// fold into a dense non-empty base literal
function extend() {
  var y = [10, 20];
  y[2] = 30;
  y[3] = 40;
  return y;
}
console.log(JSON.stringify(extend()), extend().length);

// sparse gap: only the contiguous prefix folds; the hole at index 1 is preserved
function sparse() {
  var x = [];
  x[0] = 1;
  x[2] = 3;
  return [JSON.stringify(x), x.length, 1 in x];
}
console.log(JSON.stringify(sparse())); // ["[1,null,3]",3,false]

// out-of-order / overwrite: not foldable, evaluation order preserved
var order = [];
function ooo() {
  var x = [];
  x[1] = (order.push("a"), "a");
  x[0] = (order.push("b"), "b");
  return [JSON.stringify(x), order.join(",")];
}
console.log(JSON.stringify(ooo())); // ['["b","a"]',"a,b"]

// self-reference must not fold: var x = [x] reads hoisted undefined
function selfref() {
  var x = [];
  x[0] = x;
  x[1] = 2;
  return x[0] === x && x[1] === 2;
}
console.log(selfref()); // true

// throwing store inside try: catch still sees the partial array [1]
function partial() {
  var x = [];
  try {
    x[0] = 1;
    x[1] = (() => { throw new Error("boom"); })();
    x[2] = 3;
  } catch (e) {}
  return JSON.stringify(x);
}
console.log(partial()); // [1]

// top-level (script) var: only literal RHS folds
var top = [];
top[0] = 1;
top[1] = 2;
console.log(JSON.stringify(top), top.length);
