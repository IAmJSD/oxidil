// dead-store elimination and inlining hazards
// store that looks dead but is read via getter side-effect ordering
var order=[];function f(){var t=(order.push(`a`),1);t=(order.push(`b`),2);return t}console.log(f(),order.join(`,`));
// inlining a function that references arguments / this
function wrap(e){return e+1}console.log(wrap(wrap(wrap(0))));
// inlining with default param + side effects
var c=0;function inc(){c++;return c}function use(e){return e*10}console.log(use(inc())+use(inc()));console.log(`c`,c);
// store eliminated but variable later read in catch
function risky(){var e=`init`;try{e=`assigned`;JSON.parse(`{bad`);e=`never`}catch(t){return e}}console.log(risky());
// self-assignment / no-op stores
function noop(){var e=5;return e}console.log(noop());