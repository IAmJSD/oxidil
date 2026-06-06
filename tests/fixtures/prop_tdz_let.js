function m() {
  try { console.log(typeof x); }
  catch (e) { console.log('TDZ:' + e.constructor.name); }
  let x = 5;
  return x;
}
m();
