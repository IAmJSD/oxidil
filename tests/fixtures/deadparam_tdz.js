function outer() {
  function f(a, b) { return a; }
  try { console.log(f(1, q)); }
  catch (e) { console.log('threw: ' + e.constructor.name); }
  let q = 5;
}
outer();
