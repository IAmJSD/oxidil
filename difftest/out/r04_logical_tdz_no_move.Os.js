// REGRESSION: `true && VAL` simplifies to `VAL`, but no pass may move that TDZ
// read of a later-declared const past its initializer.
function e(){const e=t;const t=7;return e}try{console.log(e())}catch(e){console.log(`logical-tdz:`,e.constructor.name)}