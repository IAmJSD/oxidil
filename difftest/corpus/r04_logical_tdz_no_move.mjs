// REGRESSION: `true && VAL` simplifies to `VAL`, but no pass may move that TDZ
// read of a later-declared const past its initializer.
function probe() {
  const x = (true && VAL);  // TDZ read of VAL
  const VAL = 7;            // eslint-disable-line
  return x;
}
try {
  console.log(probe());
} catch (e) {
  console.log("logical-tdz:", e.constructor.name);
}
