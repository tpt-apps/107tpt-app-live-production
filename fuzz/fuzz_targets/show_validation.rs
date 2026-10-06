#![no_main]
// Validation over arbitrary parseable-ish structures must be total.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(file) = tpt_app_live_production_model::showfile::ShowFile::from_str(
        &String::from_utf8_lossy(data),
    ) {
        if let Ok(show) = tpt_app_live_production_model::Show::try_from(file) {
            let report = tpt_app_live_production_core::headless::validate_show(&show);
            // Errors and warnings are the only severities.
            for issue in &report.issues {
                assert!(issue.severity == "error" || issue.severity == "warning");
            }
        }
    }
});
