// typeof/coercion edge cases: +0/-0, NaN, "" + n, null/undefined
console.log(Object.is(+0, -0)); // false
console.log(+0 === -0); // true
console.log(1 / +0, 1 / -0); // Infinity, -Infinity
console.log(0 * -1); // -0
console.log(1 / (0 * -1)); // -Infinity  (-0 hazard)
console.log("" + (0 * -1)); // "0"
console.log(NaN === NaN); // false
console.log(typeof NaN); // number
console.log("" + null, "" + undefined); // "null", "undefined"
console.log(null == undefined, null === undefined); // true false
console.log(typeof null, typeof undefined); // object undefined
console.log("" + 1 + 2, 1 + 2 + ""); // "12" "3"
console.log([] + [], [] + {}, {} + []); // "" "[object Object]" "[object Object]"
console.log(+"", +"  ", +"0x10", +"1e3"); // 0 0 16 1000
console.log(typeof (() => {}), typeof function(){}, typeof class {}); // function x3
console.log(0.1 + 0.2); // 0.30000000000000004
console.log(9007199254740993); // precision
console.log(-0); // 0 printed
console.log((-0).toString(), String(-0)); // "0" "0"
var x = -0;
console.log(x === 0, Object.is(x, -0));
