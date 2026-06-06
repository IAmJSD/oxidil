function f(x: number) {
  if (typeof x === "string") return "got string at runtime";
  return "treated as non-string";
}
console.log(f("hello" as any));
