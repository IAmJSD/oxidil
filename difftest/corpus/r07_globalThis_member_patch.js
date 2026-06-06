// REGRESSION: writing a property of a global-object alias (globalThis.Math = 1)
// replaces the global Math; pure-eval must NOT fold Math.abs(-1) afterward.
globalThis.Math = 1;
try {
  console.log(Math.abs(-1));
  console.log("no-throw");
} catch (e) {
  console.log("threw:", e.constructor.name);
}
