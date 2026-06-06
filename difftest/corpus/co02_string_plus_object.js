// Object with side-effecting toString in repeated string concatenation.
let count = 0;
const o = { toString() { count++; return "x"; } };

let r1 = "p" + o;          // count 1
let r2 = "p" + o;          // count 2
let r3 = "p" + o + o;      // count 4
console.log(r1, r2, r3, count);   // px px pxx 4

let acc = "";
for (let i = 0; i < 4; i++) {
  acc += "q" + o;          // count +1 each -> 8
}
console.log(acc, count);   // qxqxqxqx 8
