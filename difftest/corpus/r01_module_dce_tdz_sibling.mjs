// REGRESSION: DCE must not delete an UNUSED const whose initializer is a TDZ
// read of a later-declared sibling lexical binding. `unused` is genuinely dead
// (never read), so DCE is tempted to drop it -- but evaluating its initializer
// `later` throws ReferenceError (TDZ), which must be preserved.
function probe() {
  try {
    const unused = later;   // dead binding, but the TDZ read must still throw
  } catch (e) {
    return "dce-tdz:" + e.constructor.name;
  }
  const later = 5;          // eslint-disable-line
  return "no-throw";
}
console.log(probe());
