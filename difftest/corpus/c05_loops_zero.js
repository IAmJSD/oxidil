// loops including zero-iteration, loop-invariant exprs, while/do-while/for-of/for-in
var sum = 0;
for (var i = 0; i < 0; i++) { sum += 1; } // zero iterations
console.log("zero-for", sum);

var k = 0;
while (k < 0) { k++; } // zero
console.log("zero-while", k);

var d = 0;
do { d++; } while (d < 0); // runs once
console.log("do-while", d);

// loop-invariant pure expression inside loop
var total = 0;
var base = 10;
for (var n = 0; n < 4; n++) {
  total += base * 2 + n; // base*2 invariant
}
console.log("invariant", total);

// for-of and for-in order
var arr = [3, 1, 2];
var col = [];
for (var v of arr) col.push(v);
console.log("for-of", col.join(","));

var o = { b: 1, a: 2, c: 3 };
var keys = [];
for (var key in o) keys.push(key);
console.log("for-in", keys.join(","));

// loop with side-effecting invariant
var calls = 0;
function side() { calls++; return 2; }
var t2 = 0;
for (var m = 0; m < 3; m++) { t2 += side(); }
console.log("side-invariant", t2, calls);
