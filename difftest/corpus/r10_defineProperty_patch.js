// REGRESSION: Object.defineProperty(Math, "floor", ...) monkey-patches Math via
// an argument, not an assignment target; pure-eval must not fold Math.floor.
// parseInt is unaffected and must still fold.
Object.defineProperty(Math, "floor", { value: function (x) { return 999; } });
console.log(parseInt("42"), Math.floor(3.7));
