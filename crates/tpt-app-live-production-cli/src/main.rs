//! `tpt-live-production` — CLI for show-file validation and headless
//! rehearsal (spec 16).
//!
//! The exit-code contract is stable and must not change between releases:
//!
//! | code | meaning            |
//! |------|--------------------|
//! | 0    | SUCCESS            |
//! | 1    | WARNINGS           |
//! | 2    | VALIDATION_FAILED  |
//! | 3    | CONFIGURATION_ERROR|
//! | 4    | INPUT_ERROR        |
//! | 5    | INTERNAL_ERROR     |

use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use serde::Serialize;
use tpt_app_live_production_core::headless::{
    rehearse_headless, validate_show, IssueRecord, RehearsalOptions, RehearsalResult,
};
use tpt_app_live_production_model::showfile::ShowFile;

/// Exit codes (spec 16). Stable contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    /// Everything passed.
    Success = 0,
    /// Validation found warnings only; the operation ran.
    Warnings = 1,
    /// Validation found errors; the operation did not run.
    ValidationFailed = 2,
    /// Bad invocation or configuration.
    ConfigurationError = 3,
    /// The input file is missing, unreadable, or unparsable.
    InputError = 4,
    /// An unexpected internal failure.
    InternalError = 5,
}

impl From<ExitStatus> for ExitCode {
    fn from(s: ExitStatus) -> Self {
        ExitCode::from(s as u8)
    }
}

/// TPT Live Production — switch / mix / cue / run the show live.
#[derive(Parser)]
#[command(
    name = "tpt-live-production",
    version,
    about = "Validate and rehearse TPT Live Production show files",
    after_help = "Exit codes: 0 success, 1 warnings, 2 validation failed, 3 configuration error, 4 input error, 5 internal error."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate a show file without running anything.
    Validate {
        /// Path to the .tptshow file.
        #[arg(long)]
        show: String,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Headlessly rehearse the show's cue stack (no outputs, simulated clock).
    Rehearse {
        /// Path to the .tptshow file.
        #[arg(long)]
        show: String,
        /// Run without any interactive output (required; reserved for
        /// future interactive rehearsal).
        #[arg(long, default_value_t = true)]
        headless: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Cap on simulated show time, in seconds.
        #[arg(long, default_value_t = 600)]
        max_seconds: u64,
    },
}

/// Machine-readable validate result (shape mirrors the spec example).
#[derive(Serialize)]
struct ValidateResult<'a> {
    show: &'a str,
    cues: usize,
    issues: usize,
    errors: usize,
    warnings: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    details: Vec<IssueRecord>,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // CLI usage problems are CONFIGURATION_ERROR (3) per the
            // exit-code contract — clap's default 2 would collide with
            // VALIDATION_FAILED.
            let _ = e.print();
            return ExitStatus::ConfigurationError.into();
        }
    };
    match run(cli) {
        Ok(status) => status.into(),
        Err(status) => status.into(),
    }
}

fn run(cli: Cli) -> Result<ExitStatus, ExitStatus> {
    match cli.command {
        Command::Validate { show, json } => cmd_validate(&show, json),
        Command::Rehearse {
            show,
            headless,
            json,
            max_seconds,
        } => {
            let _ = headless; // always headless today
            cmd_rehearse(&show, json, max_seconds)
        }
    }
}

fn load_show(path: &str) -> Result<tpt_app_live_production_model::Show, ExitStatus> {
    let file = ShowFile::load(path).map_err(|e| {
        eprintln!("error: {e}");
        match e {
            tpt_app_live_production_model::ShowFileError::Io { .. } => ExitStatus::InputError,
            tpt_app_live_production_model::ShowFileError::Parse(_)
            | tpt_app_live_production_model::ShowFileError::Encode(_)
            | tpt_app_live_production_model::ShowFileError::UnsupportedSchema { .. } => {
                ExitStatus::InputError
            }
            tpt_app_live_production_model::ShowFileError::WriteIo { .. } => {
                ExitStatus::InternalError
            }
        }
    })?;
    tpt_app_live_production_model::Show::try_from(file).map_err(|e| {
        eprintln!("error: {e}");
        ExitStatus::InputError
    })
}

fn cmd_validate(path: &str, json: bool) -> Result<ExitStatus, ExitStatus> {
    let show = load_show(path)?;
    let issues = validate_show(&show);
    let errors = issues.iter().filter(|i| i.severity == "error").count();
    let warnings = issues.len() - errors;

    let result = ValidateResult {
        show: &show.name,
        cues: show.cue_stack.len(),
        issues: issues.len(),
        errors,
        warnings,
        details: issues.clone(),
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).unwrap_or_default()
        );
    } else {
        println!("show:     {}", show.name);
        println!("cues:     {}", show.cue_stack.len());
        println!(
            "issues:   {} ({} errors, {} warnings)",
            issues.len(),
            errors,
            warnings
        );
        for issue in &issues {
            println!(
                "  [{}] {}{}: {}",
                issue.severity,
                issue.code,
                issue.cue.map(|c| format!(" cue {c}")).unwrap_or_default(),
                issue.message
            );
        }
    }

    if errors > 0 {
        Ok(ExitStatus::ValidationFailed)
    } else if warnings > 0 {
        Ok(ExitStatus::Warnings)
    } else {
        Ok(ExitStatus::Success)
    }
}

fn cmd_rehearse(path: &str, json: bool, max_seconds: u64) -> Result<ExitStatus, ExitStatus> {
    let show = load_show(path)?;

    // Validation errors abort before running (warnings do not).
    let issues = validate_show(&show);
    let errors = issues.iter().filter(|i| i.severity == "error").count();
    if errors > 0 {
        if json {
            let result = serde_json::json!({
                "show": show.name,
                "cues": show.cue_stack.len(),
                "issues": issues.len(),
                "completed": false,
                "details": issues,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&result).unwrap_or_default()
            );
        } else {
            eprintln!("error: show failed validation with {errors} error(s); not rehearsing");
            for issue in &issues {
                eprintln!("  [{}] {}: {}", issue.severity, issue.code, issue.message);
            }
        }
        return Ok(ExitStatus::ValidationFailed);
    }
    let has_warnings = !issues.is_empty();

    let options = RehearsalOptions {
        max_duration: std::time::Duration::from_secs(max_seconds.max(1)),
        ..RehearsalOptions::default()
    };
    let result: RehearsalResult = rehearse_headless(show, options).map_err(|e| {
        eprintln!("error: rehearsal failed: {e}");
        ExitStatus::InternalError
    })?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).unwrap_or_default()
        );
    } else {
        println!("show:       {}", result.show);
        println!("cues:       {} fired of {} in stack", result.fired.len(), {
            // total cues is embedded in the fired count only when complete;
            // the model length is what validate reported.
            result.cues
        });
        println!("completed:  {}", result.completed);
        println!("program:    {}", result.final_program);
        for cue in &result.fired {
            println!(
                "  cue {:>3}  {:<24} at {:>6} ms  -> {}{}",
                cue.number,
                cue.label,
                cue.fired_at_ms,
                cue.program_after,
                cue.lighting
                    .as_ref()
                    .map(|l| format!("  [lighting: {l}]"))
                    .unwrap_or_default()
            );
        }
        if !result.issues.is_empty() {
            println!("warnings:   {}", result.issues.len());
            for issue in &result.issues {
                println!("  [{}] {}: {}", issue.severity, issue.code, issue.message);
            }
        }
    }

    if !result.completed {
        eprintln!("error: rehearsal did not complete the cue stack");
        return Ok(ExitStatus::InternalError);
    }
    if has_warnings {
        Ok(ExitStatus::Warnings)
    } else {
        Ok(ExitStatus::Success)
    }
}

// Keep CommandFactory linked so clap's derive stays honest about the
// command shape used in error rendering above.
#[allow(dead_code)]
fn assert_cli_shape() {
    let _ = Cli::command().print_help();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_values_match_the_contract() {
        assert_eq!(ExitStatus::Success as u8, 0);
        assert_eq!(ExitStatus::Warnings as u8, 1);
        assert_eq!(ExitStatus::ValidationFailed as u8, 2);
        assert_eq!(ExitStatus::ConfigurationError as u8, 3);
        assert_eq!(ExitStatus::InputError as u8, 4);
        assert_eq!(ExitStatus::InternalError as u8, 5);
    }

    #[test]
    fn validate_clean_show_exits_success() {
        let dir = std::env::temp_dir().join(format!("tpt-cli-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("clean.tptshow");
        std::fs::write(&path, CLEAN_SHOW).unwrap();
        let status = cmd_validate(path.to_str().unwrap(), true).unwrap();
        assert_eq!(status, ExitStatus::Success);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_warnings_exit_one() {
        let dir = std::env::temp_dir().join(format!("tpt-cli-w{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("warn.tptshow");
        // Instant ramp -> warning.
        std::fs::write(
            &path,
            CLEAN_SHOW.replace("muted = false }", "muted = false, ramp = \"instant\" }"),
        )
        .unwrap();
        let status = cmd_validate(path.to_str().unwrap(), false).unwrap();
        assert_eq!(status, ExitStatus::Warnings);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_errors_exit_two() {
        let dir = std::env::temp_dir().join(format!("tpt-cli-e{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("bad.tptshow");
        // Cue references a source that does not exist.
        // The fade cue now targets a source that does not exist.
        std::fs::write(
            &path,
            CLEAN_SHOW.replace("source = \"cam2\",", "source = \"ghost\","),
        )
        .unwrap();
        let status = cmd_validate(path.to_str().unwrap(), false).unwrap();
        assert_eq!(status, ExitStatus::ValidationFailed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_input_error() {
        let status = cmd_validate("Z:/definitely/not/here.tptshow", false).unwrap_err();
        assert_eq!(status, ExitStatus::InputError);
    }

    #[test]
    fn malformed_file_is_input_error() {
        let dir = std::env::temp_dir().join(format!("tpt-cli-m{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("malformed.tptshow");
        std::fs::write(&path, "this is [[ not toml").unwrap();
        let status = cmd_validate(path.to_str().unwrap(), false).unwrap_err();
        assert_eq!(status, ExitStatus::InputError);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rehearse_clean_show_exits_success() {
        let dir = std::env::temp_dir().join(format!("tpt-cli-r{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("clean.tptshow");
        std::fs::write(&path, CLEAN_SHOW).unwrap();
        let status = cmd_rehearse(path.to_str().unwrap(), false, 60).unwrap();
        assert_eq!(status, ExitStatus::Success);
        let _ = std::fs::remove_dir_all(&dir);
    }

    const CLEAN_SHOW: &str = r#"
schema_version = 1
name = "CLI Test"

[settings]
video_fps = 60

[[outputs]]
id = "pgm_video"
kind = "program_video"

[[outputs]]
id = "pa"
kind = "program_audio"

[[sources]]
id = "cam1"
kind = "live_video_input"

[[sources]]
id = "cam2"
kind = "live_video_input"

[[sources]]
id = "mic1"
kind = "live_audio_input"

[[buses]]
id = "program"
label = "Program"
output = "pa"
inputs = [{ source = "mic1", gain_db = 0.0 }]

[[cues]]
number = 1
label = "Open"
video = { transition = "cut", source = "cam1" }
audio = [{ action = "set_mute", source = "mic1", muted = false }]

[[cues]]
number = 2
label = "Alt"
video = { transition = "fade", source = "cam2", duration_ms = 500 }
advance = { mode = "timed", after_ms = 1000 }

[[cues]]
number = 3
label = "Back"
video = { transition = "cut", source = "cam1" }
"#;
}
