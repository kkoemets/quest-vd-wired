//! Testable v4 host building blocks.
//!
//! The network parsers are deliberately pure and bounded so the same entry
//! points can be exercised by property tests and `cargo-fuzz` without opening
//! sockets or invoking ADB.

pub mod adb;
pub mod control;
pub mod diagnostics;
pub mod embedded;
pub mod protocol;
pub mod runtime;
pub mod socks;
pub mod state;
pub mod udp;
