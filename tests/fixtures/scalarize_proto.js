// Boundary: `Object.prototype.b` is defined, so a missing `b` key reads the
// inherited value (7), not `undefined`. The pass must bail on the prototype
// mutation rather than fill `void 0`.
Object.defineProperty(Object.prototype, "b", { configurable: true, get() { return 7; } });
function build() {
  function f(opts) { return opts.a + opts.b; }
  return f({ a: 1 });
}
console.log(build());
delete Object.prototype.b;
