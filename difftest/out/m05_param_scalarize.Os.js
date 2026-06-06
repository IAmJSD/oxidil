// param-scalarization in a module: top-level non-exported helpers are split into
// scalar params; exported / escaping ones are left alone.
function e(e,t){return e+t}console.log(e(1,2),e(10,20));
// missing keys fill `void 0` -> default expressions still work
function t(e,t){return(t||`hi`)+` `+(e||`?`)}console.log(t(`x`,void 0),t(`y`,`yo`));
// value mutation of a key stays correct (fresh object, never escapes)
function n(e){e+=1;return e}console.log(n(4));
// exported binding: must NOT be split (importers call it)
export function pub(e){return e*2}console.log(pub(21));
// escape: returned object must NOT be split
function r(e){return e}console.log(JSON.stringify(r({a:1,b:2})));