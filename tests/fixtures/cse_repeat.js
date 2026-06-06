function f(a, b) {
  const x = a;
  const y = b;
  const p = (x === y) && (x !== 0);
  const q = (x === y) && (x !== 0);
  const r = (x === y) && (x !== 0);
  return [p, q, r];
}
console.log(JSON.stringify(f(1, 1)));
console.log(JSON.stringify(f(1, 2)));
console.log(JSON.stringify(f(0, 0)));
