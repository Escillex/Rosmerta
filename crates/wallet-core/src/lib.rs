//! Wallet model, budget maths and last-write-wins merge. Pure Rust, no I/O:
//! time and ids come in through [`Env`] so storage and the UI stay outside.

pub mod book;
pub mod budget;
pub mod date;
pub mod model;
pub mod money;
pub mod sync;

pub use book::{Book, Change, Env};
pub use date::Date;
pub use model::{Budget, Kind, Theme, Transaction, Wallet};
