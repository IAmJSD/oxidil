// reassignment, dead stores, propagation hazards
function f() {
  var a = 1;
  a = 2;       // dead store? but...
  var b = a;   // propagate
  a = 3;       // a reassigned after
  return b + a; // 2 + 3 = 5
}
console.log(f());

function g() {
  var x = 10;
  var y = x;   // y=10
  x = 20;      // does NOT change y
  return [x, y];
}
console.log(g().join(","));

// reassignment via compound, ++ etc
function h() {
  var c = 0;
  c += 5;
  c *= 2;
  c--;
  return c; // 9
}
console.log(h());

// dead store that is actually observed via closure
function k() {
  var z = 1;
  var read = () => z;
  z = 99;
  return read(); // 99
}
console.log(k());

// var hoisting / TDZ-ish (var)
function m() {
  console.log(typeof undef_v); // undefined
  var undef_v = 5;
  return undef_v;
}
console.log(m());
