// closures & captured vars, loop-captured vars, single-use vars
function makeCounters(){var e=[];for(let t=0;t<3;t++){e.push(function(){return t})}var t=0;for(var n=0;n<3;n++){(function(t){e.push(function(){return t*10})})(n)}return e}var fns=makeCounters();for(var a=0;a<fns.length;a++)console.log(fns[a]());function adder(e){var t=123;return function(t){return e+t}}var add5=adder(5);console.log(add5(10),add5(-2));
// single-use var that is also captured
function f(){var e=`captured`;return()=>e+`!`}console.log(f()());