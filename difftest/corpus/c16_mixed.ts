// TS mixed: enums, type assertions, generics erased, plus runtime behavior
interface Point { x: number; y: number; }
function dist(p: Point): number { return Math.abs(p.x) + Math.abs(p.y); }
console.log(dist({ x: -3, y: 4 }));

const arr: number[] = [1, 2, 3];
const doubled = arr.map((v: number): number => v * 2);
console.log(doubled.join(","));

type Pair<T> = [T, T];
const pr: Pair<string> = ["a", "b"];
console.log(pr.join("-"));

// const assertion + typeof
const k = "key" as const;
console.log(typeof k === "string" ? "k string" : "no");

// non-null assertion, optional chaining, nullish
const maybe: { v?: number } = {};
console.log(maybe.v ?? -1);
console.log(maybe?.v);

// class with private-ish fields and methods
class Counter {
  count = 0;
  inc(): this { this.count++; return this; }
}
const ctr = new Counter();
ctr.inc().inc().inc();
console.log(ctr.count);

// satisfies / as on numbers
const big = 9007199254740991 as number;
console.log(big + 1);
