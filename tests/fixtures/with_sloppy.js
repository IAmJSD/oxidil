function f(obj) {
  var zzz = 10;
  with (obj) {
    return zzz;
  }
}
