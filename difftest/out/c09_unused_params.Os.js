// functions with unused trailing params (dead_param), arguments object, arity
function f(e,t,n){return e+t}console.log(f(1,2,3));console.log(f.length);
// arguments must reflect all passed args even if params unused
function g(e,t){return arguments.length+`:`+Array.prototype.join.call(arguments,`-`)}console.log(g(1,2,3,4));
// unused param that has a side-effecting default expr
var calls=0;function side(){calls++;return 7}function h(e,t=side()){return e}console.log(h(1));console.log(h(1,100));console.log(`calls`,calls);
// callee uses arguments via fn.length elsewhere
function variadic(){return arguments.length}console.log(variadic(),variadic(1,2,3));
// param reassigned
function reassignParam(e){e=e+1;return e}console.log(reassignParam(41));