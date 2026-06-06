// ES module: top-level const/let folding/elimination is sound at module root
// (module-private bindings, invisible to sibling scripts). Exported bindings
// keep their slot but their reads may be propagated within the module.
export const tag = "v1";
// Re-read the export within the module (exercise that the export slot survives).
console.log("v1");
console.log(10, 15, 30);
// TDZ at module root must be preserved: a pre-declaration read still throws.
try {
	console.log(later);
} catch (e) {
	console.log("tdz:", e.constructor.name);
}
const later = 42;
console.log(later);
// Top-level `var` must NOT be touched even in a module.
var hoisted = 7;
console.log(hoisted);
