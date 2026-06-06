// dead-store elimination and inlining hazards
// store that looks dead but is read via getter side-effect ordering
var order = [];
function f() {
	var a = (order.push("a"), 1);
	a = (order.push("b"), 2);
	return a;
}
console.log(f(), order.join(","));
// inlining a function that references arguments / this
function wrap(x) {
	return x + 1;
}
console.log(wrap(wrap(wrap(0))));
// inlining with default param + side effects
var c = 0;
function inc() {
	c++;
	return c;
}
function use(v) {
	return v * 10;
}
console.log(use(inc()) + use(inc()));
console.log("c", c);
// store eliminated but variable later read in catch
function risky() {
	var result = "init";
	try {
		result = "assigned";
		JSON.parse("{bad");
		result = "never";
	} catch (e) {
		return result;
	}
}
console.log(risky());
// self-assignment / no-op stores
function noop() {
	var x = 5;
	x = x;
	return x;
}
console.log(noop());
