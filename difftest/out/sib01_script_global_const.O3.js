// NEGATIVE test for sibling-script global visibility.
// In a .js SCRIPT (not a module), top-level `const` lives on the script's
// global lexical scope and is visible to sibling scripts loaded in the same
// realm. oxidil intentionally does NOT module-optimize scripts, so this
// top-level const must NOT be root-inlined/eliminated even though it is used
// exactly once. We can't load a sibling script in this single-file harness,
// so this is an output-equivalence (negative) test: behavior must match O0.
const SHARED = 21;
console.log(SHARED);
// Re-reference indirectly to make the single-use real, not a dead store.
console.log(typeof SHARED);
