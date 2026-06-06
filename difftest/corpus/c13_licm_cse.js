// targeted at LICM / CSE / GVN soundness with impure invariants and aliasing
var arr = [1, 2, 3, 4];

// loop-invariant member that aliases the loop body's writes
var out = [];
for (var i = 0; i < arr.length; i++) {
  arr.push(0);           // mutates arr -> arr.length changes if hoisted wrongly
  out.push(i);
  if (out.length > 10) break; // safety
}
console.log("len-grow", out.join(","), arr.length);

// invariant function call that throws on a later iteration -> ordering matters
function maybeThrow(n) { if (n === 3) throw new Error("at3"); return n; }
var collected = [];
try {
  for (var j = 0; j < 5; j++) {
    collected.push(maybeThrow(j));
  }
} catch (e) {
  collected.push("err:" + e.message);
}
console.log(collected.join(","));

// CSE candidate where subexpr reads a let captured/mutated in loop
var acc = 0;
var p = { v: 1 };
for (var k = 0; k < 3; k++) {
  acc += p.v + p.v;  // p.v read twice; p.v mutated each iter
  p.v = p.v * 2;
}
console.log("cse-mut", acc); // 1+1 + 2+2 + 4+4 = 14

// division by loop-varying value, -0 / NaN hazards
var res = [];
for (var z = -1; z <= 1; z++) {
  res.push(1 / (z * 0)); // z*0 -> -0,0,0 => -Inf, Inf, Inf  (wait z=-1 -> -0)
}
console.log(res.join(","));
