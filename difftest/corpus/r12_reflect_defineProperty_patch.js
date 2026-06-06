// REGRESSION: Reflect.defineProperty(Math, "abs", ...) patches Math.
Reflect.defineProperty(Math, "abs", { value: () => -1 });
console.log(Math.abs(5));
