// REGRESSION: single-use inlining must not move a TDZ-reading initializer past
// the later-declared binding's init (would convert a throw into a value).
function e(){const e=t;const t=5;return e}try{console.log(e())}catch(e){console.log(`inline-tdz:`,e.constructor.name)}