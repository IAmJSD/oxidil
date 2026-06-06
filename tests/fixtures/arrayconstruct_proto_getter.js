// Soundness boundary: a getter on Array.prototype["0"] makes `x[0] = 1` a [[Set]]
// that hits the inherited accessor (no setter -> sloppy no-op), while a folded
// `[1]` literal would define an own element. The pass must detect the
// Array.prototype mutation and refuse to fold, so every level prints "GET".
Object.defineProperty(Array.prototype, "0", {
  configurable: true,
  get() { return "GET"; },
});
function f() {
  var x = [];
  x[0] = 1;
  x[1] = 2;
  return [x[0], x[1], x.length];
}
console.log(JSON.stringify(f()));
delete Array.prototype[0];
