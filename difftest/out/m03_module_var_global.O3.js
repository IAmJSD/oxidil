// Top-level `var` in a module must NOT be propagated/inlined (conservative).
var v = 1;
function read() {
	return v;
}
console.log(read());
v = 2;
console.log(read());
console.log(v);
