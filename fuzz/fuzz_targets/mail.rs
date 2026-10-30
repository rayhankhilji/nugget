//! RFC 822 / MIME mail (`.eml`), as the files connector hands it to `nugget-extract`.
#![no_main]

use libfuzzer_sys::fuzz_target;

#[path = "extract_common.rs"]
mod extract_common;

fuzz_target!(|data: &[u8]| extract_common::run("message/rfc822", data));
