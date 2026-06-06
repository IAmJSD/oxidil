// Exported binding with a side-effecting initializer; re-read must be identical.
// Also an unused-but-exported function must NOT be DCE'd.
let e=0;function t(){e++;return e*10}export const x=t();console.log(x);console.log(x===10);console.log(e);export function f(){return`f-alive`}console.log(typeof f,f());