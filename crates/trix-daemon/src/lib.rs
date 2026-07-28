//! Library surface for `trix-daemon`: the pipe transport and the client
//! registry.
//!
//! Not in the brief's literal file list, but required to satisfy the
//! project's zero-warnings bar honestly. `clients::Clients` has real
//! behaviour and real tests (`stats.subscribe` fan-out, dead-receiver
//! eviction) that this task deliberately does not wire into `main.rs`'s
//! dispatch loop yet — Task 5 does that. As plain `mod`s of a bin-only
//! crate, `cargo build`'s dead-code pass (which walks reachability from
//! `fn main` only) would flag every `Clients` method `main.rs` does not yet
//! call. Declaring them `pub mod` here instead makes them this crate's
//! public library surface — exactly what the task brief's Interfaces line
//! already called them ("Produces: `pipe::{PIPE_NAME, serve}`,
//! `clients::{Clients, ClientId}`") — so the compiler treats them as part of
//! the crate's external contract rather than as dead internals. This also
//! makes `trix-daemon` structurally consistent with the rest of the
//! workspace: `trix-core` and `trix-proto` are libraries with a thin
//! consumer (`trix-cli`); this gives the daemon binary the same shape.

pub mod clients;
pub mod pipe;
