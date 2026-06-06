// Getter with side effect in repeated member expressions.
// Member exprs in repeated positions must NEVER be CSE'd.
let c = 0;
const o = { get v() { c++; return c; } };  // returns incrementing counter

let r = o.v < o.v;          // reads: first o.v=1, second o.v=2 -> 1<2 -> true
console.log(r, c);          // true 2

let r2 = o.v + o.v + o.v;   // 3+4+5 = 12
console.log(r2, c);         // 12 5

let acc = 0;
for (let i = 0; i < 3; i++) {
  acc += o.v;               // 6,7,8 -> 21
}
console.log(acc, c);        // 21 8
