#![no_main]
// Inbound control-surface bytes may be rejected; never panic.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = tpt_av_control_osc::OscServer::parse_bytes(data);
});
