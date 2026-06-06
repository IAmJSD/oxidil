function dist(e){return Math.abs(e.x)+Math.abs(e.y)}console.log(dist({x:-3,y:4}));const arr=[1,2,3];const doubled=arr.map(e=>e*2);console.log(doubled.join(`,`));const pr=[`a`,`b`];console.log(pr.join(`-`));
// const assertion + typeof
const k=`key`;console.log(typeof k===`string`?`k string`:`no`);
// non-null assertion, optional chaining, nullish
const maybe={};console.log(maybe.v??-1);console.log(maybe?.v);
// class with private-ish fields and methods
class Counter{count=0;inc(){this.count++;return this}}const ctr=new Counter;ctr.inc().inc().inc();console.log(ctr.count);
// satisfies / as on numbers
const big=9007199254740991;console.log(big+1);