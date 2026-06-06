// switch with fallthrough, default placement, no break
function classify(n) {
  var out = [];
  switch (n) {
    case 0:
      out.push("zero");
    case 1:
      out.push("one-or-less");
      break;
    case 2:
    case 3:
      out.push("two-or-three");
    default:
      out.push("default");
      break;
    case 4:
      out.push("four");
  }
  return out.join(",");
}
for (var i = 0; i <= 5; i++) console.log(i + ":" + classify(i));

// switch on typeof
function t(x) {
  switch (typeof x) {
    case "number": return "num";
    case "string": return "str";
    default: return "other";
  }
}
console.log(t(1), t("a"), t({}), t(undefined));
