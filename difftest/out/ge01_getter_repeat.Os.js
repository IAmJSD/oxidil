// Getter with side effect in repeated member expressions.
// Member exprs in repeated positions must NEVER be CSE'd.
let c=0;const o={get v(){c++;return c}};let r=o.v<o.v;console.log(r,c);let r2=o.v+o.v+o.v;console.log(r2,c);let acc=0;for(let e=0;e<3;e++){acc+=o.v}console.log(acc,c);