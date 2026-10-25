//! RollVault core engine.

// Preflight is returned by value on purpose (it is the UI payload).
#![allow(clippy::result_large_err)]

#[macro_use]
pub mod l10n;

pub mod darwin_io;
pub mod hashing;
pub mod preset;
pub mod scan;
pub mod transfer;
pub mod ascmhl;
pub mod legacy_mhl;
pub mod util;
pub mod volumes;
pub mod scheduler;
pub mod tools;
pub mod report;
pub mod journal;
pub mod job;
pub mod reverify;
