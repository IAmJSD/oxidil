// param-scalarization in a module: top-level non-exported helpers are split into
// scalar params; exported / escaping ones are left alone.
function add(opts) {
	return opts.a + opts.b;
}
console.log(add({
	a: 1,
	b: 2
}), add({
	a: 10,
	b: 20
}));
// missing keys fill `void 0` -> default expressions still work
function greet(opts) {
	return (opts.hi || "hi") + " " + (opts.name || "?");
}
console.log(greet({ name: "x" }), greet({
	hi: "yo",
	name: "y"
}));
// value mutation of a key stays correct (fresh object, never escapes)
function bump(opts) {
	opts.n += 1;
	return opts.n;
}
console.log(bump({ n: 4 }));
// exported binding: must NOT be split (importers call it)
export function pub(opts) {
	return opts.k * 2;
}
console.log(pub({ k: 21 }));
// escape: returned object must NOT be split
function id(opts) {
	return opts;
}
console.log(JSON.stringify(id({
	a: 1,
	b: 2
})));
