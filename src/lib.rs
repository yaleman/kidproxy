#![deny(warnings)]
#![warn(unused_extern_crates)]
#![recursion_limit = "512"]
#![deny(clippy::todo)]
#![deny(clippy::unimplemented)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::panic)]
#![deny(clippy::unreachable)]
#![deny(clippy::await_holding_lock)]
#![deny(clippy::needless_pass_by_value)]
#![deny(clippy::trivially_copy_pass_by_ref)]
#![allow(clippy::large_enum_variant)]

pub mod admin;
pub mod capture;
pub mod cli;
pub mod config;
pub mod entity;
pub mod error;
pub mod event;
pub mod migration;
pub mod probe;
pub mod proxy;
pub mod runtime_manager;
pub mod tls;
pub mod transform;
pub mod writer;
