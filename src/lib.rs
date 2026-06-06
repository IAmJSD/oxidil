//! oxidil: a Rust JS/TS optimizing compiler with an oxc front-end.
//!
//! The library exposes a pure, in-memory compile core ([`driver::compile`]) that
//! drives parse -> type-strip -> optimization passes -> codegen. It performs no
//! file I/O, so it links into both the native CLI binary (`src/main.rs`) and the
//! `wasm32` build consumed by the npm package (gated behind the `wasm` feature).

pub mod driver;
pub mod error;
pub mod level;
pub mod pass;
pub mod semantic_util;
pub mod sourcemap;
pub mod ts_strip;

#[cfg(feature = "wasm")]
mod wasm;

pub use driver::{compile, CompileOptions, CompileOutput};
pub use error::{CompileError, Result};
pub use level::OptLevel;
pub use pass::Overrides;
