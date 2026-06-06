// Boundary: the key `a` collides with an outer binding `a` referenced in the
// function; naming the param `a` would capture it, so we must bail.
function build() {
  let a = 100;
  function f(opts) { return opts.a + a; }
  return f({ a: 1 });
}
console.log(build());
