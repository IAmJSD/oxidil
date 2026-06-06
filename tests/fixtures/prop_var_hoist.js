function m() { console.log(typeof undef_v, undef_v + 1); var undef_v = 5; return undef_v; }
console.log(m());
