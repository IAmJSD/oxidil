function f() {
  try { x = x; console.log('no error'); }
  catch (e) { console.log('caught:', e.constructor.name); }
  let x = 1;
}
f();
