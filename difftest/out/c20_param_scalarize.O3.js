// param-scalarization for nested (non-global) script functions, incl. recursion
// and side-effect-ordered values.
function run() {
	const log = [];
	function f(a, b, c) {
		return a * b + (c || 0);
	}
	const r1 = f(2, (log.push("b"), 3), void 0);
	const r2 = f(4, 5, 6);
	return [
		r1,
		r2,
		log.join(",")
	];
}
console.log(JSON.stringify(run()));
function outer() {
	// the recursive call is itself a rewritten call site
	function fact(n) {
		return n <= 1 ? 1 : n * fact(n - 1);
	}
	return fact(5);
}
console.log(outer());
// escape inside a script function: not split
function holder() {
	function keep(opts) {
		globalThis.__k = opts;
		return opts.v;
	}
	return keep({ v: 9 });
}
console.log(holder(), globalThis.__k.v);
