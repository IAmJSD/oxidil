// REGRESSION: a for-of loop head writing a global member (for (Math.floor of ...))
// patches Math; pure-eval must not fold Math.floor.
for (Math.floor of [() => 22]) {}
console.log(Math.floor(3.7));
