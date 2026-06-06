// repeated pure subexpressions (CSE/GVN), but with hidden side effects
function pure(a, b) { return a * b + 1; }
console.log(pure(2, 3) + pure(2, 3)); // 7+7=14, deterministic

// "pure-looking" but reads mutable outer state
var state = 5;
function reads() { return state * 2; }
console.log(reads() + reads()); // 20
state = 10;
console.log(reads() + reads()); // 40

// repeated subexpression with side effect in the middle
var seq = [];
function tick(label) { seq.push(label); return seq.length; }
var r = tick("a") + tick("b") + tick("a");
console.log(r, seq.join(",")); // 1+2+3=6, "a,b,a"

// array index repeated, array mutated between
var arr = [1, 2, 3];
function getFirst() { return arr[0]; }
console.log(getFirst() + getFirst()); // 2
arr[0] = 100;
console.log(getFirst()); // 100

// Math repeated (pure) vs Date (impure) - use deterministic only
console.log(Math.max(1, 2) + Math.max(1, 2)); // 4
