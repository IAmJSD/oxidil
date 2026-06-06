// REGRESSION: same-scope inlining must NOT fire across switch case labels.
// Entering via `case 2` jumps past the `const a = 5` init in `case 1`, so the
// read of `a` is a TDZ access that must throw.
function f(e){switch(e){case 1:const e=5;return`case1`;case 2:return e}}try{console.log(f(2))}catch(e){console.log(`switch-tdz:`,e.constructor.name)}