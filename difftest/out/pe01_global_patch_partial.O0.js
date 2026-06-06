// Per-global granular pure-eval: patching Math.floor must disable folding of
// Math.floor calls, but parseInt / Number remain foldable.
Math.floor = () => 9;
console.log(parseInt("10"), Math.floor(2.5), Number("3"));
console.log(Math.floor(7.9));
console.log(parseInt("0x1F", 16), Number("42"));
