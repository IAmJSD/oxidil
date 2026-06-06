function f(p) {
  const x = p;
  try { x = x; console.log('no error'); }
  catch (e) { console.log('caught:', e.constructor.name); }
  return x;
}
console.log(f(7));
