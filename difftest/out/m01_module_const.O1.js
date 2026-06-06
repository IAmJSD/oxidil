// Module-root const propagation/inlining/dce.
// A,B chain of consts; C is exported (binding must NOT be removed); D used once.
const A = 2;
const B = 6;
export const C = B;
const D = 5;
console.log(2, B, B);
console.log(5);
console.log(B);
