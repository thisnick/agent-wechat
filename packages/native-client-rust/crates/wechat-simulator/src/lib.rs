//! Local decoded upstream simulator for the real hub. No WeChat transport is used.
pub mod adapter;
mod contract;
pub mod engine;
pub mod service;
pub mod workspace;
pub use contract::{Control, SendFault};
