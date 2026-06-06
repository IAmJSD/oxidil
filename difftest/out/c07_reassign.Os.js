// reassignment, dead stores, propagation hazards
function f(){var e=1;e=2;var t=e;e=3;return t+e}console.log(f());function g(){var e=10;var t=e;e=20;return[e,t]}console.log(g().join(`,`));
// reassignment via compound, ++ etc
function h(){var e=0;e+=5;e*=2;e--;return e}console.log(h());
// dead store that is actually observed via closure
function k(){var e=1;var t=()=>e;e=99;return t()}console.log(k());
// var hoisting / TDZ-ish (var)
function m(){console.log(typeof e);var e=5;return e}console.log(m());