// getters/setters with side effects; repeated pure-looking subexpressions
var log=[];var obj={};var _v=0;Object.defineProperty(obj,"x",{get:function(){log.push(`get`);return _v},set:function(t){log.push(`set:`+t);_v=t},enumerable:true,configurable:true});obj.x=5;console.log(obj.x+obj.x);obj.x=obj.x+1;console.log(_v);console.log(log.join(`,`));
// repeated member access that LOOKS pure but isn't
var counter={n:0,get next(){return++this.n}};console.log(counter.next+counter.next+counter.next);
// getter that throws sometimes
var g={get bad(){throw new Error(`boom`)}};try{console.log(g.bad)}catch(e){console.log(`caught `+e.message)}