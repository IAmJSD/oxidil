// TS: const typeof fold (sound) vs param annotation (must NOT fold)
const n = 42;
const s = "hello";
const b = true;
const fn = function () { return 1; };

if (typeof n === "number") console.log("n is number");
if (typeof s === "string") console.log("s is string");
console.log(typeof b === "boolean" ? "b bool" : "b other");
console.log(typeof fn === "function" ? "fn func" : "fn other");

switch (typeof n) {
  case "number": console.log("switch number"); break;
  default: console.log("switch default");
}

// param annotation: a string is actually passed despite ": number"
function f(x: number): string {
  if (typeof x === "string") return "got string";
  return "got " + typeof x;
}
console.log(f(5 as any));
console.log(f("oops" as any)); // MUST print "got string" - annotation not enforced

// reassigned const-like via let -> poisoned
let m = 10;
m = 20;
console.log(typeof m === "number" ? "m number" : "m other");

// typeof of a const object
const o = { a: 1 };
console.log(typeof o === "object" ? "o object" : "o other");
