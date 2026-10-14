//! C89 preprocessor, lexer and parser for the original DOOM source,
//! producing `wsdoom::cc::ast`.

mod lex;
mod parse;
mod pp;

use std::path::{Path, PathBuf};
use wsdoom::cc::ast::Unit;

/// Preprocesses and parses one `.c` file. `#include "x"` is searched next
/// to the including file, then in `include_dirs`; `#include <x>` is skipped
/// (the plan's system-header macros and typedefs are predefined). `defines`
/// are extra object-like macros (`("NORMALUNIX", "1")`).
pub fn parse_file(
    path: &Path,
    include_dirs: &[PathBuf],
    defines: &[(&str, &str)],
) -> Result<Unit, String> {
    let mut pp = pp::Pp::new(include_dirs, defines);
    let toks = pp.run(path)?;
    parse::parse(&toks, &pp.files)
}

#[cfg(test)]
mod tests;
