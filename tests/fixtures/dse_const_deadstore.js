function f() {
  const x = 1;
  try { x = 2; console.log('no error'); }
  catch (e) { console.log('caught:', e.constructor.name); }
}
f();
