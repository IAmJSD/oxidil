// short-circuit &&/||/??, ternary, comma/sequence with side effects
var log = [];
function t(x) {
	log.push("t" + x);
	return true;
}
function fl(x) {
	log.push("f" + x);
	return false;
}
console.log(t(1) && fl(2) && t(3));
console.log(fl(4) || t(5) || fl(6));
console.log(log.join(","));
log.length = 0;
var a = null;
console.log(a ?? "default");
console.log(0);
console.log("yes");
console.log("");
// comma / sequence preserved
function f() {
	log.push("call");
	return 5;
}
var r = (f(), 99);
console.log(r, log.join(","));
// ternary with side effects in branches
log.length = 0;
var cond = false;
var x = cond ? t(7) : fl(8);
console.log(x, log.join(","));
// nested logical
console.log(2);
console.log(null);
console.log("x");
