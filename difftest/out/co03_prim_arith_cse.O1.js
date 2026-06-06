// POSITIVE case: pure primitive arithmetic on never-mutated number consts.
// Broadened CSE/LICM SHOULD fire here and stay equal across O levels.
const w = 4;
const h = 5;
let p = w * h + w * h;
console.log(p);
let q = w * h + w * h + w * h;
console.log(q);
let sum = 0;
for (let i = 0; i < 10; i++) {
	sum += w * h + w * h;
}
console.log(sum);
