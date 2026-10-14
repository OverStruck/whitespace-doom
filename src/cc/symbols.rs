//! The link between compiled C and runtime code written with the DSL
//! (libc, the I_* platform layer, W_* lump access): addresses of C globals,
//! struct layouts and function handles, all filled in by the compiler.
//!
//! Calling convention, shared by C and DSL functions:
//! - the caller pushes arguments left to right and `Call`s; the callee pops
//!   them (last first) and leaves exactly one return value (0 for void);
//! - a variadic callee additionally receives the number of variadic
//!   arguments, pushed after them (see `Prog::def_variadic`);
//! - a function pointer is the callee's label (`Fun::label`); 0 is NULL.
//!
//! Data: one cell per scalar; a pointer is a cell address; C strings hold one
//! char per cell and end with 0; unsigned values stay within 0..2^bits.

use crate::dsl::Fun;
use std::collections::HashMap;

/// A function implemented in Rust (DSL) rather than compiled from C. If C
/// also defines it, the C definition is ignored.
#[derive(Clone, Copy, Debug)]
pub struct Builtin {
    pub name: &'static str,
    /// Fixed parameters.
    pub arity: usize,
    pub variadic: bool,
}

#[derive(Default)]
pub struct Symbols {
    /// Address of every C global (including static ones, by plain name;
    /// file-static names that collide are not exposed).
    pub globals: HashMap<String, i64>,
    /// (struct tag or typedef name, field) -> cell offset.
    pub fields: HashMap<(String, String), i64>,
    /// Struct tag or typedef name -> size in cells.
    pub sizes: HashMap<String, i64>,
    /// Every callable function: C definitions and builtins.
    pub funcs: HashMap<String, Fun>,
}

impl Symbols {
    pub fn global(&self, name: &str) -> i64 {
        *self
            .globals
            .get(name)
            .unwrap_or_else(|| panic!("no C global `{name}`"))
    }

    pub fn field(&self, ty: &str, field: &str) -> i64 {
        *self
            .fields
            .get(&(ty.to_string(), field.to_string()))
            .unwrap_or_else(|| panic!("no field `{ty}.{field}`"))
    }

    pub fn size(&self, ty: &str) -> i64 {
        *self
            .sizes
            .get(ty)
            .unwrap_or_else(|| panic!("no type `{ty}`"))
    }

    pub fn func(&self, name: &str) -> Fun {
        *self
            .funcs
            .get(name)
            .unwrap_or_else(|| panic!("no function `{name}`"))
    }
}
