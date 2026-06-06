//! Compiler error type + Result alias and exit-code mapping.

use oxc_diagnostics::OxcDiagnostic;

#[derive(Debug)]
pub enum CompileError {
    /// IO failure reading input or writing outputs.
    Io(std::io::Error),
    /// Parser produced errors (or panicked). Carries the diagnostics for printing.
    ParseErrors(Vec<OxcDiagnostic>),
    /// Source map load / compose / serialize failure.
    SourceMapError(String),
}

impl From<std::io::Error> for CompileError {
    fn from(e: std::io::Error) -> Self {
        CompileError::Io(e)
    }
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::Io(e) => write!(f, "IO error: {e}"),
            CompileError::ParseErrors(errs) => {
                writeln!(f, "{} parse error(s):", errs.len())?;
                for e in errs {
                    writeln!(f, "{e}")?;
                }
                Ok(())
            }
            CompileError::SourceMapError(s) => write!(f, "source map error: {s}"),
        }
    }
}

impl std::error::Error for CompileError {}

impl CompileError {
    /// Process exit code per the CLI spec:
    /// 0 ok; 1 parse errors; 2 IO/sourcemap error; (64 is clap usage, handled by clap).
    pub fn exit_code(&self) -> i32 {
        match self {
            CompileError::ParseErrors(_) => 1,
            CompileError::Io(_) | CompileError::SourceMapError(_) => 2,
        }
    }
}

pub type Result<T> = std::result::Result<T, CompileError>;
