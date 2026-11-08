//! Incremental Rust port of vanilla DDNet 20.2.1.
//! Reference: the pinned upstream/ddnet source.
//! This is NOT yet a playable client.

#![forbid(unsafe_code)]

pub mod datafile;
pub mod network;
pub mod reliable;
pub mod transport;
pub mod game_protocol;
pub mod input;
pub mod session;
pub mod snapshot;
pub mod snapshot_delta;
pub mod interpolation;
pub mod netobjects;
pub mod huffman;
pub mod layers;
pub mod map;
pub mod graphics;
pub mod sprite_render;
pub mod texture;
pub mod map_visual;
#[cfg(feature = "window")]
pub mod window_assets;
pub mod audio;
pub mod sound_assets;
pub mod server_browser;
#[cfg(feature = "window")]
pub mod server_menu;
#[cfg(feature = "window")]
pub mod audio_device;
#[cfg(feature = "window")]
pub mod game_audio;
#[cfg(feature = "window")]
pub mod window;
#[cfg(feature = "window")]
pub mod window_online;
pub mod physics;
pub mod hook;
pub mod simulator;
pub mod world;
pub mod protocol;

pub const UPSTREAM_VERSION: &str = "20.2.1";
pub const UPSTREAM_COMMIT: &str = "176fc2f9ab85ace703c6ec10da0249b01995ba94";
