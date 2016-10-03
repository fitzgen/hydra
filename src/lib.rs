#![deny(missing_debug_implementations)]

extern crate leb128;

#[cfg(feature = "signpost")]
pub mod signpost;

pub mod simple_trace;
pub mod traits;
pub mod trace_ring_buffer;