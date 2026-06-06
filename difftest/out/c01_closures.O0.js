// closures & captured vars, loop-captured vars, single-use vars
function makeCounters() {
	var fns = [];
	for (let i = 0; i < 3; i++) {
		fns.push(function() {
			return i;
		});
	}
	var acc = 0;
	for (var j = 0; j < 3; j++) {
		(function(k) {
			fns.push(function() {
				return k * 10;
			});
		})(j);
	}
	return fns;
}
var fns = makeCounters();
for (var a = 0; a < fns.length; a++) console.log(fns[a]());
function adder(x) {
	var unused = 123;
	return function(y) {
		return x + y;
	};
}
var add5 = adder(5);
console.log(add5(10), add5(-2));
// single-use var that is also captured
function f() {
	var s = "captured";
	return () => s + "!";
}
console.log(f()());
