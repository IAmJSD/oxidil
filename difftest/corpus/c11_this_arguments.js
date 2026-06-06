// 'this' binding, arrow lexical this, method calls, arguments
var obj = {
  v: 42,
  regular: function () { return this.v; },
  arrow: function () { return (() => this.v)(); },
  detachable: function () { return this; },
};
console.log(obj.regular());
console.log(obj.arrow());

var detached = obj.detachable;
console.log(detached() === undefined || detached() === globalThis); // sloppy: global; strict: undefined

// call/apply/bind
function greet(greeting, punct) { return greeting + ", " + this.name + punct; }
var ctx = { name: "world" };
console.log(greet.call(ctx, "hi", "!"));
console.log(greet.apply(ctx, ["hey", "?"]));
var bound = greet.bind(ctx, "yo");
console.log(bound("."));

// this in nested function (sloppy)
var o2 = {
  go: function () {
    function inner() { return typeof this; }
    return inner();
  }
};
console.log(o2.go());

// arguments + this together
function acc() { return this.base + arguments.length; }
console.log(acc.call({ base: 100 }, 1, 2, 3));
