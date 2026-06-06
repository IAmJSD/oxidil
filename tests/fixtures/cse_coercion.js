const o = { valueOf() { console.log('coerce!'); return 1; } };
const k = 2;
const a = (o < k) === (k < k);
const b = (o < k) === (k < k);
console.log(a, b);
