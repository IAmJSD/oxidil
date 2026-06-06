// Object with side-effecting valueOf in repeated arithmetic.
// Any unsound CSE/LICM that dedups `o+o` would change calls.length.
let calls=[];const o={valueOf(){calls.push(1);return 2}};let a=o+o+o;console.log(a,calls.length);
// Same `o+o` appearing 2+ times straight-line.
let s1=o+o;let s2=o+o;console.log(s1,s2,calls.length);
// Same `o+o` inside a loop (loop-invariant operand, but NOT hoistable: side effects).
let total=0;for(let e=0;e<3;e++){total+=o+o}console.log(total,calls.length);