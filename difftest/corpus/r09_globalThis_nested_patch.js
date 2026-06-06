// REGRESSION: globalThis.Math.floor = f patches the global Math (immediate
// property of the alias); pure-eval must not fold Math.floor.
globalThis.Math.floor = function (x) { return 333; };
console.log(Math.floor(3.7));
