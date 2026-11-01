//! Drive the Logitech MX Creative Keypad (`046d:c354`): images on its nine LCD
//! keys, presses of those keys and its two page buttons, and panel brightness.
//!
//! The wire protocol is ported from OpenLogi (MIT/Apache-2.0), re-expressed
//! against blocking `hidapi`; see `PROTOCOL.md`. [`config`] reads the JSON
//! setup the `mx-keypad` binary runs.

pub mod blink;
pub mod config;
pub mod device;
pub mod geometry;
mod hidpp;
pub mod image;
pub mod input;
pub mod keys;
pub mod press;
pub mod render;
pub mod shift;
mod vendor;

pub use device::Keypad;
pub use image::KeyImageJpeg;
pub use input::{Button, Event};
