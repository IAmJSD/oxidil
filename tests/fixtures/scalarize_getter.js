// Boundary: a getter in the call-site literal is invoked once per `opts.a` read
// (twice here). Splitting to a value param would read it once, so we must bail.
function build() {
  let calls = 0;
  function f(opts) { return opts.a + opts.a; }
  const r = f({ get a() { calls++; return 5; } });
  return [r, calls];
}
console.log(JSON.stringify(build()));
