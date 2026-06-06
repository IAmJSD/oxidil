// REGRESSION: a global member written inside a destructuring pattern
// ([Math.floor] = [f]) patches Math; pure-eval must not fold Math.floor.
[Math.floor]=[function(e){return 555}];console.log(Math.floor(3.7));