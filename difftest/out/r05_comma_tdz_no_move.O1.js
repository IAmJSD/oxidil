// REGRESSION: `(0, VAL)` must keep throwing TDZ; no pass may move the read.
function probe() {
	const x = (0, VAL);
	const VAL = 8;
	return x;
}
try {
	console.log(probe());
} catch (e) {
	console.log("comma-tdz:", e.constructor.name);
}
