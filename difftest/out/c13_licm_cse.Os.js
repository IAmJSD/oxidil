// targeted at LICM / CSE / GVN soundness with impure invariants and aliasing
var arr=[1,2,3,4];
// loop-invariant member that aliases the loop body's writes
var out=[];for(var i=0;i<arr.length;i++){arr.push(0);out.push(i);if(out.length>10)break}console.log(`len-grow`,out.join(`,`),arr.length);
// invariant function call that throws on a later iteration -> ordering matters
function maybeThrow(e){if(e===3)throw new Error(`at3`);return e}var collected=[];try{for(var j=0;j<5;j++){collected.push(maybeThrow(j))}}catch(e){collected.push(`err:`+e.message)}console.log(collected.join(`,`));
// CSE candidate where subexpr reads a let captured/mutated in loop
var acc=0;var p={v:1};for(var k=0;k<3;k++){acc+=p.v+p.v;p.v=p.v*2}console.log(`cse-mut`,acc);
// division by loop-varying value, -0 / NaN hazards
var res=[];for(var z=-1;z<=1;z++){res.push(1/(z*0))}console.log(res.join(`,`));