// TypeScript sample to exercise ts_strip + folding.
interface Point {
  x: number;
  y: number;
}
type Id = string | number;

const n: number = 2 * 21;
const s = ("hello" as string) + " world";
const v = (42 as Id)!;

function origin(): Point {
  return { x: 0, y: 0 };
}

console.log(n, s, v, origin());
