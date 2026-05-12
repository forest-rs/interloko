//! Milestone-0 Cairn compiler frontend.
//!
//! This crate intentionally implements only the first vertical slice from the
//! repository design: a tiny Dylan-like syntax lowered to direct
//! `execution_tape` functions.

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

/// Syntax and parsing support.
pub mod syntax;
