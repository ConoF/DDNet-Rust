//! Incremental Rust port of vanilla DDNet 20.2.1.
//! Reference: the pinned upstream/ddnet source.
//! This is NOT yet a playable client.

#![forbid(unsafe_code)]

pub mod datafile;
pub mod network;
pub mod reliable;
pub mod transport;
pub mod game_protocol;
pub mod session;
pub mod huffman;
pub mod layers;
pub mod map;
pub mod physics;
pub mod protocol;

pub const UPSTREAM_VERSION: &str = "20.2.1";
pub const UPSTREAM_COMMIT: &str = "176fc2f9ab85ace703c6ec10da0249b01995ba94";
