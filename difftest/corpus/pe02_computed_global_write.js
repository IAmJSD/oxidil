// Computed global write: optimizer can't statically know which global is
// clobbered, so pure-eval must fall back to a whole-pass bail (no fold).
// If pure-eval wrongly folded Math.abs(-1) -> 1, the catch would NOT fire.
globalThis["Ma" + "th"] = 1;
try {
  console.log(Math.abs(-1));   // Math is now the number 1; 1.abs is not a function -> TypeError
  console.log("no-throw");
} catch (e) {
  console.log("threw:", e.constructor.name);   // TypeError at every O level
}
