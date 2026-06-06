// Soundness boundary: a getter installed on Object.prototype makes `x.gp = 123`
// a [[Set]] that hits the inherited accessor (no setter -> sloppy no-op), whereas
// a folded `{gp: 123}` literal would CreateDataProperty an own value. The pass
// must detect the Object.prototype mutation and refuse to fold, so O0 and the
// optimized levels print identically ("GET", not 123).
Object.defineProperty(Object.prototype, "gp", {
  configurable: true,
  get() { return "GET"; },
});
function f() {
  var x = {};
  x.gp = 123;
  x.other = 7;
  return [x.gp, x.other];
}
console.log(JSON.stringify(f()));
delete Object.prototype.gp;
