// recursion, mutual recursion, named function expressions
function fact(t){return t<=1?1:t*fact(t-1)}console.log(fact(5),fact(0),fact(1));function fib(e){return e<2?e:fib(e-1)+fib(e-2)}console.log(fib(10));
// mutual recursion
function isEven(e){return e===0?true:isOdd(e-1)}function isOdd(e){return e===0?false:isEven(e-1)}console.log(isEven(10),isOdd(7));
// named function expression referencing itself
var f=function e(t){return t<=0?0:t+e(t-1)};console.log(f(4));
// recursion via accumulator with closure
function sumTo(e){function t(n,r){return n>e?r:t(n+1,r+n)}return t(1,0)}console.log(sumTo(10));