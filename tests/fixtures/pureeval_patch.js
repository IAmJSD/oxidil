let log = [];
Math.floor = function (x) { log.push("floor"); return 0; };
let r = Math.floor(9.9);
console.log(JSON.stringify({ r, log }));
