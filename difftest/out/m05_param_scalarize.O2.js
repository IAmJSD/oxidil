// param-scalarization in a module: top-level non-exported helpers are split into
// scalar params; exported / escaping ones are left alone.
function add(a, b) {
	return a + b;
}
console.log(add(1, 2), add(10, 20));
// missing keys fill `void 0` -> default expressions still work
function greet(name, hi) {
	return (hi || "hi") + " " + (name || "?");
}
console.log(greet("x", void 0), greet("y", "yo"));
// value mutation of a key stays correct (fresh object, never escapes)
function bump(n) {
	n += 1;
	return n;
}
console.log(bump(4));
// exported binding: must NOT be split (importers call it)
export function pub(k) {
	return k * 2;
}
console.log(pub(21));
// escape: returned object must NOT be split
function id(opts) {
	return opts;
}
console.log(JSON.stringify(id({
	a: 1,
	b: 2
})));
