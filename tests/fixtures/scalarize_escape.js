// Boundary: the options object escapes (is returned), so it must NOT be split.
function build() {
  function f(opts) { return opts; }
  const o = f({ a: 1, b: 2 });
  return [o.a, o.b, typeof o];
}
console.log(JSON.stringify(build()));
