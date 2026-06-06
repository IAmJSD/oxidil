// REGRESSION: DCE must not delete an UNUSED const whose initializer reads an
// undeclared global (a read that throws ReferenceError: not defined).
function e(){try{const e=notDefinedAnywhere}catch(e){return`dce-undeclared:`+e.constructor.name}return`no-throw`}console.log(e());