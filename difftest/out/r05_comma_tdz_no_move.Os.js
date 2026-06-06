// REGRESSION: `(0, VAL)` must keep throwing TDZ; no pass may move the read.
function e(){const e=(0,t);const t=8;return e}try{console.log(e())}catch(e){console.log(`comma-tdz:`,e.constructor.name)}