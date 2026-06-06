// 'this' binding, arrow lexical this, method calls, arguments
var obj={v:42,regular:function(){return this.v},arrow:function(){return(()=>this.v)()},detachable:function(){return this}};console.log(obj.regular());console.log(obj.arrow());var detached=obj.detachable;console.log(detached()===undefined||detached()===globalThis);
// call/apply/bind
function greet(e,t){return e+`, `+this.name+t}var ctx={name:`world`};console.log(greet.call(ctx,`hi`,`!`));console.log(greet.apply(ctx,[`hey`,`?`]));var bound=greet.bind(ctx,`yo`);console.log(bound(`.`));
// this in nested function (sloppy)
var o2={go:function(){function e(){return typeof this}return e()}};console.log(o2.go());
// arguments + this together
function acc(){return this.base+arguments.length}console.log(acc.call({base:100},1,2,3));