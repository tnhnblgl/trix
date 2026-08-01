//! Library surface for `trix-daemon`: the daemon's state, the dispatcher that
//! drives it, the pipe transport, and the client registry.
//!
//! Not in the brief's literal file list, but required to satisfy the
//! project's zero-warnings bar honestly. Several of these have real behaviour
//! and real tests (`stats.subscribe` fan-out, overflow eviction, the status
//! wire shape) ahead of the command that will call them — `stats.subscribe`
//! itself lands in Task 7. As plain `mod`s of a bin-only crate, `cargo build`'s
//! dead-code pass (which walks reachability from `fn main` only) would flag
//! every method `main.rs` does not yet reach. Declaring them `pub mod` here
//! instead makes them this crate's public library surface — exactly what the
//! task briefs' Interfaces lines already call them ("Produces:
//! `pipe::{PIPE_NAME, serve}`, `clients::{Clients, ClientId}`",
//! "Produces: `state::Daemon`") — so the compiler treats them as part of the
//! crate's external contract rather than as dead internals. It is also what
//! lets `tests/` drive the transport directly. This makes `trix-daemon`
//! structurally consistent with the rest of the workspace: `trix-core` and
//! `trix-proto` are libraries with a thin consumer (`trix-cli`); this gives the
//! daemon binary the same shape.

pub mod autostart;
pub mod clients;
pub mod dispatch;
pub mod pipe;
pub mod state;
pub mod stats;
pub mod tray;
pub mod window;
