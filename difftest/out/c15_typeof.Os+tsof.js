// TS: const typeof fold (sound) vs param annotation (must NOT fold)
const n=42;const s=`hello`;const b=true;const fn=function(){return 1};console.log(`n is number`);console.log(`s is string`);console.log(`b bool`);console.log(`fn func`);switch(true){case true:console.log(`switch number`);break;default:console.log(`switch default`)}
// param annotation: a string is actually passed despite ": number"
function f(e){if(typeof e===`string`)return`got string`;return`got `+typeof e}console.log(f(5));console.log(f(`oops`));
// reassigned const-like via let -> poisoned
let m=10;m=20;console.log(typeof m===`number`?`m number`:`m other`);
// typeof of a const object
const o={a:1};console.log(`o object`);