// Module-root TDZ / dominance must be respected.
// `typeof y` before a lexical `const y` is a TDZ access -> ReferenceError
// (typeof does NOT shield lexical bindings, unlike truly-undeclared globals).
try{console.log(typeof e)}catch(e){console.log(`tdz1:`,e.constructor.name)}const e=1;console.log(e);
// A block with an earlier read of a later-declared inner const.
{try{console.log(e)}catch(e){console.log(`tdz2:`,e.constructor.name)}const e=99;console.log(e)}