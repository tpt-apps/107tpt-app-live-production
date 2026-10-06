//! Tauri desktop shell for TPT Live Production (spec 15).
//!
//! The shell hosts the [`LiveEngine`] and exposes operator commands to the
//! web frontend. The real-time video surface (PVW/PGM monitors) is a
//! dedicated native render surface distinct from the app chrome — the
//! frontend renders source labels/transition state; frame pixels come from
//! the video backend through the engine's `FrameOutputs`.
//!
//! Build with the Tauri CLI:
//!
//! ```text
//! cargo install tauri-cli --version ^2
//! cd crates/tpt-app-live-production-tauri
//! tauri dev    # or: tauri build
//! ```
//!
//! The crate is excluded from the workspace default build because the
//! Tauri stack is heavy and needs the frontend bundle present.

use std::sync::Mutex;

use tauri::State;
use tpt_app_live_production_core::engine::{LiveEngine, ShowState};
use tpt_app_live_production_model::cue::Transition;
use tpt_app_live_production_model::ids::SourceId;
use tpt_app_live_production_model::OperationMode;

/// Shared engine handle.
pub struct AppState {
    pub engine: Mutex<LiveEngine>,
}

/// Serializable view of engine state for the frontend.
#[derive(serde::Serialize)]
pub struct UiState {
    #[serde(flatten)]
    state: ShowState,
    /// Whether program delivery is gated (false in rehearsal).
    program_active: bool,
}

#[tauri::command]
fn get_state(state: State<AppState>) -> Result<UiState, String> {
    let engine = state.engine.lock().map_err(|e| e.to_string())?;
    Ok(UiState {
        program_active: engine.program_audio_active(),
        state: engine.state(),
    })
}

#[tauri::command]
fn go(state: State<AppState>) -> Result<Option<(u32, String)>, String> {
    let mut engine = state.engine.lock().map_err(|e| e.to_string())?;
    Ok(engine.go().map(|exec| (exec.cue.number.0, exec.cue.label)))
}

#[tauri::command]
fn take_cut(state: State<AppState>) -> Result<(), String> {
    let mut engine = state.engine.lock().map_err(|e| e.to_string())?;
    engine.take(Transition::Cut).map_err(|e| e.to_string())
}

#[tauri::command]
fn take_fade(state: State<AppState>, duration_ms: u64) -> Result<(), String> {
    let mut engine = state.engine.lock().map_err(|e| e.to_string())?;
    engine
        .take(Transition::Fade {
            duration: std::time::Duration::from_millis(duration_ms),
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn select_preview(state: State<AppState>, source: String) -> Result<(), String> {
    let mut engine = state.engine.lock().map_err(|e| e.to_string())?;
    engine
        .select_preview(SourceId::new(source))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_mode(state: State<AppState>, mode: String) -> Result<(), String> {
    let mut engine = state.engine.lock().map_err(|e| e.to_string())?;
    let mode = match mode.as_str() {
        "rehearsal" => OperationMode::Rehearsal,
        "live" => OperationMode::Live,
        other => return Err(format!("unknown mode '{other}'")),
    };
    engine.set_mode(mode);
    Ok(())
}

#[tauri::command]
fn load_show_file(state: State<AppState>, path: String) -> Result<String, String> {
    let file = tpt_app_live_production_model::showfile::ShowFile::load(&path)
        .map_err(|e| e.to_string())?;
    let show = tpt_app_live_production_model::Show::try_from(file).map_err(|e| e.to_string())?;
    let name = show.name.clone();
    let engine = LiveEngine::build(
        show,
        tpt_app_live_production_core::engine::EngineConfig::default(),
    )
    .map_err(|e| e.to_string())?;
    *state.engine.lock().map_err(|e| e.to_string())? = engine;
    Ok(name)
}

#[cfg(mobile)]
tauri::mobile_entry_point!(run);

pub fn run() {
    // The engine starts from a show file passed via the UI (or the default
    // empty show); production wiring attaches the session log, watchdog,
    // OSC surface, sACN sink, and the frame/audio backends here.
    let show = tpt_app_live_production_model::Show::new("default", "Untitled Show");
    let engine = LiveEngine::build(
        show,
        tpt_app_live_production_core::engine::EngineConfig::default(),
    )
    .expect("default show builds");
    tauri::Builder::default()
        .manage(AppState {
            engine: Mutex::new(engine),
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            go,
            take_cut,
            take_fade,
            select_preview,
            set_mode,
            load_show_file
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
