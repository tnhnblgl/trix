//! Wire types for the Trix control protocol.
//!
//! Pure serde — no windows-rs, no unsafe, no engine types. The daemon exposes
//! every capability it has over a named pipe speaking newline-delimited JSON;
//! this crate is the convenience layer for clients that happen to be written
//! in Rust. A UI in C#, Python, or TypeScript reimplements these definitions
//! and is in no way second-class.
//!
//! See `docs/superpowers/specs/2026-07-26-trix-desktop-ui-design.md` §4.

pub mod command;
pub mod message;

pub use command::{Command, DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT};
pub use message::{
    ClipMeta, Event, MAX_LINE_BYTES, RESERVED_ID, Request, Response, ShotMeta, decode_request,
    encode_line,
};
