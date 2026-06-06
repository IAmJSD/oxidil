// REGRESSION: single-use inlining must not move a TDZ-reading initializer past
// the later-declared binding's init (would convert a throw into a value).
function probe() {
  const a = later;        // TDZ read of `later` -> ReferenceError
  const later = 5;        // eslint-disable-line
  return a;
}
try {
  console.log(probe());
} catch (e) {
  console.log("inline-tdz:", e.constructor.name);
}
