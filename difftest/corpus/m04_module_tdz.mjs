// Module-root TDZ / dominance must be respected.
// `typeof y` before a lexical `const y` is a TDZ access -> ReferenceError
// (typeof does NOT shield lexical bindings, unlike truly-undeclared globals).
try {
  console.log(typeof y);
} catch (e) {
  console.log("tdz1:", e.constructor.name);
}
const y = 1;
console.log(y);

// A block with an earlier read of a later-declared inner const.
{
  try {
    console.log(z);
  } catch (e) {
    console.log("tdz2:", e.constructor.name);
  }
  const z = 99;
  console.log(z);
}
