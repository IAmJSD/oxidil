// Top-level `var` in a module must NOT be propagated/inlined (conservative).
var v = 1;
function read() { return v; }
console.log(read());     // 1
v = 2;
console.log(read());     // 2 -- proves v was not const-propagated
console.log(v);
