// object-construction folding: a freshly-declared object built up by a run of
// own-property stores becomes one literal — plus the soundness boundaries that
// must keep the original behavior.
// basic fold: literal + effectful (non-throwing) RHS, computed literal keys
function build(e){var t={a:1,b:e(),["c-d"]:3,[0]:`zero`};return t}console.log(JSON.stringify(build(()=>2)),build(()=>2)[`c-d`]);
// fold into a non-empty base literal (spread + data prop)
function extend(e){var t={...e,base:true,extra:9};return t}console.log(JSON.stringify(extend({k:1})));
// self-reference must NOT fold: var x = {self:x} would read hoisted undefined
function selfref(){var e={};e.self=e;e.n=5;return e.self===e&&e.n===5}console.log(selfref());
// inside try, a possibly-throwing store must not be folded past the throw:
// the catch still sees the partially-built object {a:1}
function partial(){var e={};try{e.a=1;e.b=(()=>{throw new Error(`boom`)})();e.c=3}catch(e){}return JSON.stringify(e)}console.log(partial());
// __proto__ store is the prototype accessor, never a plain own-property define
function protoStore(e){var t={};t.__proto__=e;t.v=1;return Object.getPrototypeOf(t)===e&&t.v===1}console.log(protoStore({inherited:7}));
// top-level (script) var: only literal RHS folds; a throw would leave a global
var top={k:1,j:2};console.log(JSON.stringify(top));