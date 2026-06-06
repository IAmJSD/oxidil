// reassignment, dead stores, propagation hazards
function f() {
	var a = 1;
	a = 2;
	var b = a;
	a = 3;
	return b + a;
}
console.log(f());
function g() {
	var x = 10;
	var y = x;
	x = 20;
	return [x, y];
}
console.log(g().join(","));
// reassignment via compound, ++ etc
function h() {
	var c = 0;
	c += 5;
	c *= 2;
	c--;
	return c;
}
console.log(h());
// dead store that is actually observed via closure
function k() {
	var z = 1;
	var read = () => z;
	z = 99;
	return read();
}
console.log(k());
// var hoisting / TDZ-ish (var)
function m() {
	console.log(typeof undef_v);
	var undef_v = 5;
	return undef_v;
}
console.log(m());
