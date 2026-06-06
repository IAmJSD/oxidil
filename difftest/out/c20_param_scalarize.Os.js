// param-scalarization for nested (non-global) script functions, incl. recursion
// and side-effect-ordered values.
function run(){const e=[];function t(e,t,n){return e*t+(n||0)}const n=t(2,(e.push(`b`),3),void 0);const r=t(4,5,6);return[n,r,e.join(`,`)]}console.log(JSON.stringify(run()));function outer(){
// the recursive call is itself a rewritten call site
function e(t){return t<=1?1:t*e(t-1)}return e(5)}console.log(outer());
// escape inside a script function: not split
function holder(){function e(e){globalThis.__k=e;return e.v}return e({v:9})}console.log(holder(),globalThis.__k.v);