// object-construction folding: a freshly-declared object built up by a run of
// own-property stores becomes one literal — plus the soundness boundaries that
// must keep the original behavior.
// basic fold: literal + effectful (non-throwing) RHS, computed literal keys
function build(g) {
	var x = {
		a: 1,
		b: g(),
		["c-d"]: 3,
		[0]: "zero"
	};
	return x;
}
console.log(JSON.stringify(build(() => 2)), build(() => 2)["c-d"]);
// fold into a non-empty base literal (spread + data prop)
function extend(src) {
	var y = {
		...src,
		base: true,
		extra: 9
	};
	return y;
}
console.log(JSON.stringify(extend({ k: 1 })));
// self-reference must NOT fold: var x = {self:x} would read hoisted undefined
function selfref() {
	var x = {};
	x.self = x;
	x.n = 5;
	return x.self === x && x.n === 5;
}
console.log(selfref());
// inside try, a possibly-throwing store must not be folded past the throw:
// the catch still sees the partially-built object {a:1}
function partial() {
	var x = {};
	try {
		x.a = 1;
		x.b = (() => {
			throw new Error("boom");
		})();
		x.c = 3;
	} catch (e) {}
	return JSON.stringify(x);
}
console.log(partial());
// __proto__ store is the prototype accessor, never a plain own-property define
function protoStore(p) {
	var x = {};
	x.__proto__ = p;
	x.v = 1;
	return Object.getPrototypeOf(x) === p && x.v === 1;
}
console.log(protoStore({ inherited: 7 }));
// top-level (script) var: only literal RHS folds; a throw would leave a global
var top = {
	k: 1,
	j: 2
};
console.log(JSON.stringify(top));
