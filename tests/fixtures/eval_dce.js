function f() {
  let secret = 42;
  function helper() { return 7; }
  return eval("secret + helper()");
}
console.log(f());
