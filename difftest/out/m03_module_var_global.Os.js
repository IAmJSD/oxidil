// Top-level `var` in a module must NOT be propagated/inlined (conservative).
var e=1;function t(){return e}console.log(t());e=2;console.log(t());console.log(e);