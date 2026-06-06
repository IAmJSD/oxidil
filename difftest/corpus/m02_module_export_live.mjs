// Exported binding with a side-effecting initializer; re-read must be identical.
// Also an unused-but-exported function must NOT be DCE'd.
let n = 0;
function sideEffect() { n++; return n * 10; }
export const x = sideEffect();
console.log(x);          // re-read exported x
console.log(x === 10);   // value identity
console.log(n);          // sideEffect must have run exactly once
export function f() { return "f-alive"; }   // unused but exported: keep it
console.log(typeof f, f());
