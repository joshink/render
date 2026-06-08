//! # render-poc
//!
//! A headless GPU-accelerated video rendering engine. Reads a JSON render
//! specification describing compositions of media clips, solid layers, text,
//! effects, and transitions, then uses WebGPU compute shaders to produce
//! the final output image or video.
//!
//! ## Architecture
//!
//! - [`config`] — Deserializes the JSON spec and evaluates runtime expressions/keyframes.
//! - [`engine`] — Owns the GPU render pipeline: texture management, shader dispatch, and CPU readback.

pub mod config;
pub mod engine;
pub mod layout;

