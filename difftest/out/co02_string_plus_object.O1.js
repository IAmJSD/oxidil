// Object with side-effecting toString in repeated string concatenation.
let count = 0;
const o = { toString() {
	count++;
	return "x";
} };
let r1 = "p" + o;
let r2 = "p" + o;
let r3 = "p" + o + o;
console.log(r1, r2, r3, count);
let acc = "";
for (let i = 0; i < 4; i++) {
	acc += "q" + o;
}
console.log(acc, count);
