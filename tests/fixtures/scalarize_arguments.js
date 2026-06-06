// Boundary: the function observes `arguments`; splitting changes the arity it
// sees (1 object arg -> 2 scalar args), so we must bail.
function build() {
  function f(opts) { return opts.a + opts.b + arguments.length; }
  return f({ a: 1, b: 2 });
}
console.log(build());
