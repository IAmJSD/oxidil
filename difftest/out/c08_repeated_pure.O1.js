// repeated pure subexpressions (CSE/GVN), but with hidden side effects
function pure(a, b) {
	return a * b + 1;
}
console.log(pure(2, 3) + pure(2, 3));
// "pure-looking" but reads mutable outer state
var state = 5;
function reads() {
	return state * 2;
}
console.log(reads() + reads());
state = 10;
console.log(reads() + reads());
// repeated subexpression with side effect in the middle
var seq = [];
function tick(label) {
	seq.push(label);
	return seq.length;
}
var r = tick("a") + tick("b") + tick("a");
console.log(r, seq.join(","));
// array index repeated, array mutated between
var arr = [
	1,
	2,
	3
];
function getFirst() {
	return arr[0];
}
console.log(getFirst() + getFirst());
arr[0] = 100;
console.log(getFirst());
// Math repeated (pure) vs Date (impure) - use deterministic only
console.log(4);
