// Object with side-effecting valueOf in repeated arithmetic.
// Any unsound CSE/LICM that dedups `o+o` would change calls.length.
let calls = [];
const o = { valueOf() { calls.push(1); return 2; } };

let a = o + o + o;          // 3 valueOf calls
console.log(a, calls.length);   // 6 3

// Same `o+o` appearing 2+ times straight-line.
let s1 = o + o;             // +2
let s2 = o + o;             // +2
console.log(s1, s2, calls.length);  // 4 4 7

// Same `o+o` inside a loop (loop-invariant operand, but NOT hoistable: side effects).
let total = 0;
for (let i = 0; i < 3; i++) {
  total += o + o;          // +2 each iteration -> +6
}
console.log(total, calls.length);   // 12 13
