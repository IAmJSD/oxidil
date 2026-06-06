// switch with fallthrough, default placement, no break
function classify(e){var n=[];switch(e){case 0:n.push(`zero`);case 1:n.push(`one-or-less`);break;case 2:case 3:n.push(`two-or-three`);default:n.push(`default`);break;case 4:n.push(`four`)}return n.join(`,`)}for(var i=0;i<=5;i++)console.log(i+`:`+classify(i));
// switch on typeof
function t(e){switch(typeof e){case`number`:return`num`;case`string`:return`str`;default:return`other`}}console.log(t(1),t(`a`),t({}),t(undefined));