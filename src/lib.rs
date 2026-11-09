//! Everything that doesn't touch hardware, tested on the host (see the README). Per SPEC.md this
//! grows the RTC ring, sample packing, timestamp freezing, manifest merge, version compare and the
//! QWP record encoding for `room_readings` and `device_status`.
#![cfg_attr(not(test), no_std)]

pub use qwp_embedded as qwp;
