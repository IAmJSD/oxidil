// recursion, mutual recursion, named function expressions
function fact(n) {
	return n <= 1 ? 1 : n * fact(n - 1);
}
console.log(fact(5), fact(0), fact(1));
function fib(n) {
	return n < 2 ? n : fib(n - 1) + fib(n - 2);
}
console.log(fib(10));
// mutual recursion
function isEven(n) {
	return n === 0 ? true : isOdd(n - 1);
}
function isOdd(n) {
	return n === 0 ? false : isEven(n - 1);
}
console.log(isEven(10), isOdd(7));
// named function expression referencing itself
var f = function self(n) {
	return n <= 0 ? 0 : n + self(n - 1);
};
console.log(f(4));
// recursion via accumulator with closure
function sumTo(n) {
	function helper(i, acc) {
		return i > n ? acc : helper(i + 1, acc + i);
	}
	return helper(1, 0);
}
console.log(sumTo(10));
