// REGRESSION: Object.assign(Math, {...}) patches Math via an argument.
Object.assign(Math,{floor:()=>42});console.log(Math.floor(3.7));