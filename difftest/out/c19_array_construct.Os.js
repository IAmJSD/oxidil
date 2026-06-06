// array-construction folding: an ascending, dense, contiguous run of indexed
// stores becomes one array literal — plus the soundness boundaries.
// basic fold: literal + effectful (non-throwing) RHS
function build(e){var t=[1,e(),`z`];return t}console.log(JSON.stringify(build(()=>2)),build(()=>2).length);
// fold into a dense non-empty base literal
function extend(){var e=[10,20,30,40];return e}console.log(JSON.stringify(extend()),extend().length);
// sparse gap: only the contiguous prefix folds; the hole at index 1 is preserved
function sparse(){var e=[1];e[2]=3;return[JSON.stringify(e),e.length,1 in e]}console.log(JSON.stringify(sparse()));
// out-of-order / overwrite: not foldable, evaluation order preserved
var order=[];function ooo(){var e=[];e[1]=(order.push(`a`),`a`);e[0]=(order.push(`b`),`b`);return[JSON.stringify(e),order.join(`,`)]}console.log(JSON.stringify(ooo()));
// self-reference must not fold: var x = [x] reads hoisted undefined
function selfref(){var e=[];e[0]=e;e[1]=2;return e[0]===e&&e[1]===2}console.log(selfref());
// throwing store inside try: catch still sees the partial array [1]
function partial(){var e=[];try{e[0]=1;e[1]=(()=>{throw new Error(`boom`)})();e[2]=3}catch(e){}return JSON.stringify(e)}console.log(partial());
// top-level (script) var: only literal RHS folds
var top=[1,2];console.log(JSON.stringify(top),top.length);