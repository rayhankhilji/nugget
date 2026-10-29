//! JSON-RPC framing over the local socket: line splitting, message decoding, reply encoding.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| nugget_ipc::fuzz_frames(data));
