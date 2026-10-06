#![no_main]
// The show-file parser may reject input; it must never panic or hang.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(file) = tpt_app_live_production_model::showfile::ShowFile::from_str(
        &String::from_utf8_lossy(data),
    ) {
        // Parseable files must also convert + validate totally.
        if let Ok(show) = tpt_app_live_production_model::Show::try_from(file) {
            let _ = tpt_app_live_production_core::headless::validate_show(&show);
        }
    }
});
