// Module-root const propagation/inlining/dce.
// A,B chain of consts; C is exported (binding must NOT be removed); D used once.
const A = 2;
const B = A * 3;
export const C = B;
const D = 5;
console.log(A, B, C);
console.log(D);
console.log(C);
