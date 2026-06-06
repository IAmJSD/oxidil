// try/catch/finally control-flow, finally overriding return, thrown errors
function f1(){try{return`try`}finally{return`finally`}}console.log(f1());function f2(){var e=[];try{e.push(`a`);throw new Error(`x`)}catch(t){e.push(`catch:`+t.message);return e.join(`,`)}finally{e.push(`fin`)}}console.log(f2());function f3(){for(var e=0;e<5;e++){try{if(e===2)continue;if(e===4)break}finally{console.log(`fin`+e)}console.log(`body`+e)}}f3();
// rethrow
function f4(){try{try{throw new TypeError(`inner`)}finally{console.log(`inner finally`)}}catch(e){return e.constructor.name}}console.log(f4());