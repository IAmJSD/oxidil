// try/catch/finally control-flow, finally overriding return, thrown errors
function f1() {
  try {
    return "try";
  } finally {
    return "finally"; // overrides
  }
}
console.log(f1());

function f2() {
  var out = [];
  try {
    out.push("a");
    throw new Error("x");
    out.push("unreachable");
  } catch (e) {
    out.push("catch:" + e.message);
    return out.join(",");
  } finally {
    out.push("fin");
  }
}
console.log(f2());

function f3() {
  for (var i = 0; i < 5; i++) {
    try {
      if (i === 2) continue;
      if (i === 4) break;
    } finally {
      console.log("fin" + i);
    }
    console.log("body" + i);
  }
}
f3();

// rethrow
function f4() {
  try {
    try { throw new TypeError("inner"); }
    finally { console.log("inner finally"); }
  } catch (e) { return e.constructor.name; }
}
console.log(f4());
