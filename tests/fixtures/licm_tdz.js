function side() { return 1; }
function f() {
  let r = [];
  for (let i = 0; i < 0; i++) {
    let x = a === b === false;
    r.push(x); r.push(x);
  }
  const a = side();
  const b = side();
  return r;
}
console.log(JSON.stringify(f()));
