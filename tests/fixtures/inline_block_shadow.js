function f(p) {
  const a = p;
  let q = 5; q++;
  {
    let p = 100;
    p++;
    return a + p;
  }
}
console.log(f(10));
