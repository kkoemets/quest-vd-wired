#![no_main]

use gnirehtet_vd::protocol::Frame;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: &[u8]| {
    let _ = Frame::decode(input);
});
