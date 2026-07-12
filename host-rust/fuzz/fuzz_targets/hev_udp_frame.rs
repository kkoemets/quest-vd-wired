#![no_main]

use gnirehtet_vd::udp::HevUdpFrame;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: &[u8]| {
    let _ = HevUdpFrame::decode(input);
});
