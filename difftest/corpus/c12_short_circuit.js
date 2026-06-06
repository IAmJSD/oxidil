// short-circuit &&/||/??, ternary, comma/sequence with side effects
var log = [];
function t(x) { log.push("t" + x); return true; }
function fl(x) { log.push("f" + x); return false; }

console.log(t(1) && fl(2) && t(3)); // false, evaluates t1,f2 only
console.log(fl(4) || t(5) || fl(6)); // true, evaluates f4,t5
console.log(log.join(","));

log.length = 0;
var a = null;
console.log(a ?? "default"); // default
console.log(0 ?? "no"); // 0 (?? only null/undef)
console.log(0 || "yes"); // yes
console.log("" && "skip"); // ""

// comma / sequence preserved
function f() { log.push("call"); return 5; }
var r = (f(), 99);
console.log(r, log.join(",")); // 99, "call"

// ternary with side effects in branches
log.length = 0;
var cond = false;
var x = cond ? t(7) : fl(8);
console.log(x, log.join(",")); // false, f8

// nested logical
console.log((1 && 2) || (3 && 0)); // 2
console.log(null && undefined); // null
console.log(NaN || "x"); // x
