//! Trix capture engine.
//!
//! Hardware-accelerated screen capture, H.264/AAC encode, an in-RAM replay
//! ring, and MP4 muxing — with no opinion about what drives it. The CLI and
//! the daemon are peers, both consuming this crate.

pub mod capture;
pub mod config;
pub mod control;
pub mod encode;
pub mod library;
pub mod probe;
pub mod record;
pub mod replay;
pub mod stats;
