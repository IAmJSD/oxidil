function f() {
  let a = 1;
  let b = a;
  a = 5;
  return b;
}
console.log(f());
