// REGRESSION: globalThis["Math"] = 1 (literal computed key) replaces Math;
// pure-eval must record the LEAF name, not the alias root.
globalThis["Math"] = 1;
try {
  console.log(Math.abs(-1));
  console.log("no-throw");
} catch (e) {
  console.log("threw:", e.constructor.name);
}
