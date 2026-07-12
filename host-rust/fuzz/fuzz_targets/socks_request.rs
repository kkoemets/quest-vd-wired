#![no_main]

use gnirehtet_vd::socks::SocksRequest;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: &[u8]| {
    let _ = SocksRequest::decode(input);
});
