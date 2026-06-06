// functions with unused trailing params (dead_param), arguments object, arity
function f(a, b, c) { return a + b; } // c unused & trailing
console.log(f(1, 2, 3));
console.log(f.length); // 3 - must stay

// arguments must reflect all passed args even if params unused
function g(a, b) {
  return arguments.length + ":" + Array.prototype.join.call(arguments, "-");
}
console.log(g(1, 2, 3, 4));

// unused param that has a side-effecting default expr
var calls = 0;
function side() { calls++; return 7; }
function h(a, b = side()) { return a; } // b unused but default has side effect when omitted
console.log(h(1));       // calls default? b omitted -> default runs -> calls=1
console.log(h(1, 100));  // default skipped
console.log("calls", calls);

// callee uses arguments via fn.length elsewhere
function variadic() { return arguments.length; }
console.log(variadic(), variadic(1, 2, 3));

// param reassigned
function reassignParam(x) { x = x + 1; return x; }
console.log(reassignParam(41));
