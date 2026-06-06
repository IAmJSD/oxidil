// Exported binding with a side-effecting initializer; re-read must be identical.
// Also an unused-but-exported function must NOT be DCE'd.
let n = 0;
function sideEffect() {
	n++;
	return n * 10;
}
export const x = sideEffect();
console.log(x);
console.log(x === 10);
console.log(n);
export function f() {
	return "f-alive";
}
console.log(typeof f, f());
