#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Mutex;
use studiocut_ai::{create_project as create, load_project, save_project as save, Project};
use tauri::State;
use tauri_plugin_dialog::DialogExt;

#[derive(Default)]
struct Workspace(
    Mutex<Option<Project>>,
    Mutex<studiocut_ai::timeline::History>,
    // Remembered project file: set by open/save so autosave can write
    // without opening a dialog. `None` until the project is saved once.
    Mutex<Option<std::path::PathBuf>>,
);

#[derive(serde::Serialize)]
struct OpenResult {
    project: Project,
    path: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryInfo {
    backup_path: String,
    backup_modified_ms: u64,
    project_modified_ms: u64,
}

fn analysis_snapshot(
    state: &State<Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
) -> Result<Project, String> {
    let current = state.0.lock().map_err(|e| e.to_string())?;
    let project = current.as_ref().ok_or("No project is open")?;
    if project.id != project_id || project.revision != revision {
        return Err("Project changed; run analysis again".into());
    }
    Ok(project.clone())
}

#[derive(Default)]
struct AiState {
    credentials: studiocut_ai::credentials::Credentials,
    director: studiocut_ai::director::Director,
}

#[tauri::command]
fn director_context(
    state: State<Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
) -> Result<studiocut_ai::director::Context, String> {
    Ok(studiocut_ai::director::context(&analysis_snapshot(
        &state, project_id, revision,
    )?))
}

#[tauri::command]
async fn director_credential(
    app: tauri::AppHandle,
    config: studiocut_ai::director::Config,
    key: Option<String>,
    storage: Option<studiocut_ai::credentials::Storage>,
) -> Result<(), String> {
    use tauri::Manager;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AiState>();
        let scope = config.scope()?;
        match key {
            Some(key) => state.credentials.set(
                config.provider,
                &scope,
                key,
                storage.ok_or("Choose OS or session-only storage")?,
            ),
            None => state.credentials.delete(config.provider, &scope),
        }
    })
    .await
    .map_err(|_| "Credential task failed".to_string())?
}

fn license_path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    use tauri::Manager;
    Ok(app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?
        .join("license.txt"))
}

fn license_gate(app: &tauri::AppHandle) -> Result<(), String> {
    let status = studiocut_ai::license::ensure_licensed(&license_path(app)?)?;
    if !status.licensed {
        return Err(format!(
            "License required. Activate a license for machine fingerprint {} in Settings.",
            status.fingerprint
        ));
    }
    Ok(())
}

#[tauri::command]
fn license_status(app: tauri::AppHandle) -> Result<studiocut_ai::license::LicenseStatus, String> {
    studiocut_ai::license::ensure_licensed(&license_path(&app)?)
}

#[tauri::command]
async fn license_activate(
    app: tauri::AppHandle,
    text: String,
) -> Result<studiocut_ai::license::LicenseInfo, String> {
    let path = license_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || studiocut_ai::license::activate(&path, &text))
        .await
        .map_err(|_| "License task failed".to_string())?
}

#[tauri::command]
async fn license_deactivate(app: tauri::AppHandle) -> Result<(), String> {
    let path = license_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || studiocut_ai::license::deactivate(&path))
        .await
        .map_err(|_| "License task failed".to_string())?
}

#[tauri::command]
async fn director_propose(
    app: tauri::AppHandle,
    state: State<'_, Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
    config: studiocut_ai::director::Config,
    instruction: String,
    consent: bool,
    mode: String,
    previous_proposal_id: Option<String>,
    previous_edl: Option<studiocut_ai::edl::Edl>,
    learned_preferences: Option<String>,
    vision: studiocut_ai::director::VisionRequest,
) -> Result<studiocut_ai::director::Proposal, String> {
    use tauri::Manager;
    license_gate(&app)?;
    if mode != "edit" && mode != "titles" {
        return Err("Director mode must be \"edit\" or \"titles\"".into());
    }
    // A revision needs both the previous id and its EDL; with only one
    // present, both are ignored and the proposal starts fresh.
    let previous = match (previous_proposal_id, previous_edl) {
        (Some(id), Some(edl)) => Some(studiocut_ai::director::PreviousProposal {
            summary: edl.summary.clone(),
            id,
            edl,
        }),
        _ => None,
    };
    let project = analysis_snapshot(&state, project_id, revision)?;
    tauri::async_runtime::spawn_blocking(move || {
        let ai = app.state::<AiState>();
        ai.director.propose(
            &ai.credentials,
            &project,
            config,
            instruction,
            consent,
            mode.as_str(),
            previous,
            learned_preferences,
            vision,
        )
    })
    .await
    .map_err(|_| "Director task failed".to_string())?
}

/// List chat models for a Director provider. The API key is per-call
/// only — it is never stored. Static lists (Claude/Bedrock) and the
/// empty Vertex list come with a note; see director::list_director_models.
#[tauri::command]
async fn list_director_models(
    request: studiocut_ai::director::DirectorModelsRequest,
) -> Result<studiocut_ai::director::DirectorModelsResponse, String> {
    tauri::async_runtime::spawn_blocking(move || {
        studiocut_ai::director::list_director_models(&request)
    })
    .await
    .map_err(|_| "Director model list failed".to_string())?
}

/// Grab one PNG frame from a video clip at `at_seconds` (seconds into
/// the clip) and return it as a base64 data URL.
#[tauri::command]
async fn extract_thumbnail(
    state: State<'_, Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
    clip_id: uuid::Uuid,
    at_seconds: f64,
) -> Result<String, String> {
    let project = analysis_snapshot(&state, project_id, revision)?;
    tauri::async_runtime::spawn_blocking(move || {
        studiocut_ai::thumbnail::extract_thumbnail(&project, clip_id, at_seconds)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Vision preview: whether the Director would send footage frames
/// for a vision request, and which frames, without extracting
/// anything or calling a provider.
#[tauri::command]
async fn director_vision_preview(
    state: State<'_, Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
    config: studiocut_ai::director::Config,
    vision: studiocut_ai::director::VisionRequest,
) -> Result<studiocut_ai::director::VisionPreview, String> {
    let project = analysis_snapshot(&state, project_id, revision)?;
    Ok(studiocut_ai::director::vision_preview(
        &project, &config, &vision,
    ))
}

#[tauri::command]
fn director_apply(state: State<Workspace>, edl: studiocut_ai::edl::Edl) -> Result<Project, String> {
    let mut current = state.0.lock().map_err(|e| e.to_string())?;
    let project = current.as_ref().ok_or("No project is open")?;
    let next = studiocut_ai::edl::apply(
        project,
        &edl,
        &mut *state.1.lock().map_err(|e| e.to_string())?,
    )?;
    *current = Some(next.clone());
    Ok(next)
}

/// Loudness of one clip (ffmpeg loudnorm), with a suggested gain to
/// reach −14 LUFS. Suggestion only — audio is never modified.
#[tauri::command]
async fn measure_loudness(
    state: State<'_, Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
    clip_id: uuid::Uuid,
) -> Result<studiocut_ai::agents::Loudness, String> {
    let project = analysis_snapshot(&state, project_id, revision)?;
    tauri::async_runtime::spawn_blocking(move || {
        studiocut_ai::agents::measure_loudness(&project, clip_id)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn detect_silence(
    state: State<'_, Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
    clip_id: uuid::Uuid,
    settings: studiocut_ai::analysis::SilenceSettings,
) -> Result<Vec<studiocut_ai::analysis::Cut>, String> {
    let project = analysis_snapshot(&state, project_id, revision)?;
    tauri::async_runtime::spawn_blocking(move || {
        studiocut_ai::analysis::detect(&project, clip_id, settings)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Scene detection (heuristic — ffmpeg scene filter + luma stats, no
/// ML, no face detection). Threshold 0.1–0.9, default 0.4.
#[tauri::command]
async fn detect_scenes(
    state: State<'_, Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
    clip_id: uuid::Uuid,
    threshold: f64,
) -> Result<Vec<studiocut_ai::analysis::Scene>, String> {
    let project = analysis_snapshot(&state, project_id, revision)?;
    tauri::async_runtime::spawn_blocking(move || {
        studiocut_ai::analysis::detect_scenes(&project, clip_id, threshold)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Filler-word cuts for a word-timestamped transcript. The language is
/// not passed: both the English and Hindi/Hinglish filler sets apply,
/// so a mixed transcript is covered either way.
#[tauri::command]
async fn detect_filler_cuts(
    state: State<'_, Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
    clip_id: uuid::Uuid,
    words: Vec<studiocut_ai::transcription::TimedText>,
) -> Result<Vec<studiocut_ai::analysis::Cut>, String> {
    let project = analysis_snapshot(&state, project_id, revision)?;
    tauri::async_runtime::spawn_blocking(move || {
        studiocut_ai::transcription::filler_cuts(&project, clip_id, &words, "")
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Colour scopes for one clip frame (approximate by default —
/// computed from a sample frame no wider than 320 px; a grading aid,
/// not a broadcast measurement. Full-resolution sampling analyses
/// the native frame instead).
#[tauri::command]
async fn analyze_scopes(
    state: State<'_, Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
    clip_id: uuid::Uuid,
    at_seconds: f64,
    full_resolution: bool,
) -> Result<studiocut_ai::scopes::ScopeData, String> {
    let project = analysis_snapshot(&state, project_id, revision)?;
    tauri::async_runtime::spawn_blocking(move || {
        studiocut_ai::scopes::analyze_scopes(&project, clip_id, at_seconds, full_resolution)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Proxy status for one asset. The original media is never modified.
#[tauri::command]
async fn proxy_status(
    app: tauri::AppHandle,
    state: State<'_, Workspace>,
    id: uuid::Uuid,
) -> Result<studiocut_ai::proxy::ProxyStatus, String> {
    use tauri::Manager;
    let asset = state
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .as_ref()
        .and_then(|p| p.assets.iter().find(|a| a.id == id))
        .cloned()
        .ok_or("Asset not in current project")?;
    let root = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?
        .join("media");
    Ok(tauri::async_runtime::spawn_blocking(move || {
        studiocut_ai::proxy::proxy_status(&root, &asset)
    })
    .await
    .map_err(|e| e.to_string())?)
}

/// Generate (or regenerate) the 540p proxy for a video asset. The
/// original file is only read, never written.
#[tauri::command]
async fn generate_proxy(
    app: tauri::AppHandle,
    state: State<'_, Workspace>,
    id: uuid::Uuid,
    preference: String,
) -> Result<studiocut_ai::proxy::ProxyStatus, String> {
    use tauri::Manager;
    let asset = state
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .as_ref()
        .and_then(|p| p.assets.iter().find(|a| a.id == id))
        .cloned()
        .ok_or("Asset not in current project")?;
    let root = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?
        .join("media");
    tauri::async_runtime::spawn_blocking(move || {
        studiocut_ai::proxy::generate_proxy(&root, &asset, &preference)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Delete an asset's proxy and its state record. The original media is
/// never touched.
#[tauri::command]
async fn delete_proxy(
    app: tauri::AppHandle,
    state: State<'_, Workspace>,
    id: uuid::Uuid,
) -> Result<studiocut_ai::proxy::ProxyStatus, String> {
    use tauri::Manager;
    let asset = state
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .as_ref()
        .and_then(|p| p.assets.iter().find(|a| a.id == id))
        .cloned()
        .ok_or("Asset not in current project")?;
    let root = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?
        .join("media");
    tauri::async_runtime::spawn_blocking(move || studiocut_ai::proxy::delete_proxy(&root, &asset))
        .await
        .map_err(|e| e.to_string())?
}

/// Resolve the ready proxy file for preview use. Callers fall back to
/// the original media when this errors.
#[tauri::command]
fn proxy_playback_source(
    app: tauri::AppHandle,
    state: State<Workspace>,
    id: uuid::Uuid,
) -> Result<String, String> {
    use tauri::Manager;
    let current = state.0.lock().map_err(|e| e.to_string())?;
    let asset = current
        .as_ref()
        .and_then(|p| p.assets.iter().find(|a| a.id == id))
        .ok_or("Asset not in current project")?;
    let root = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?
        .join("media");
    let path = studiocut_ai::proxy::proxy_source(&root, asset)?;
    app.asset_protocol_scope()
        .allow_file(&path)
        .map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// Speaker-turn hints for a word-timestamped transcript (heuristic —
/// pause-based, not machine learning; see ai_gaps::diarize_clip).
#[tauri::command]
async fn detect_speakers(
    state: State<'_, Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
    clip_id: uuid::Uuid,
    words: Vec<studiocut_ai::transcription::TimedText>,
    min_pause_secs: f64,
) -> Result<studiocut_ai::ai_gaps::DiarizationResult, String> {
    let project = analysis_snapshot(&state, project_id, revision)?;
    tauri::async_runtime::spawn_blocking(move || {
        studiocut_ai::ai_gaps::diarize_clip(&project, clip_id, &words, min_pause_secs)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Music beat detection (heuristic — energy-based onset detection,
/// not machine learning; see ai_gaps::detect_beats_clip).
#[tauri::command]
async fn detect_beats(
    state: State<'_, Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
    clip_id: uuid::Uuid,
) -> Result<studiocut_ai::ai_gaps::BeatResult, String> {
    let project = analysis_snapshot(&state, project_id, revision)?;
    tauri::async_runtime::spawn_blocking(move || {
        studiocut_ai::ai_gaps::detect_beats_clip(&project, clip_id)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Centre-crop reframe plan for a portrait/square target. No face
/// is detected by this plan itself; face tracking is available
/// separately (see ai_gaps::reframe_face_track_clip).
#[tauri::command]
async fn reframe_clip(
    state: State<'_, Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
    clip_id: uuid::Uuid,
    target: String,
    focus_x: Option<f64>,
    focus_y: Option<f64>,
) -> Result<studiocut_ai::ai_gaps::ReframePlan, String> {
    let project = analysis_snapshot(&state, project_id, revision)?;
    tauri::async_runtime::spawn_blocking(move || {
        studiocut_ai::ai_gaps::reframe_clip(&project, clip_id, &target, focus_x, focus_y)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Motion-weighted reframe plan for a portrait/square target: the
/// crop centres on the highest-motion region, falling back to the
/// centre crop for static frames. No face is detected or tracked by
/// this plan; face tracking is available separately (see
/// ai_gaps::reframe_face_track_clip).
#[tauri::command]
async fn reframe_motion_clip(
    state: State<'_, Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
    clip_id: uuid::Uuid,
    target: String,
    at_seconds: f64,
) -> Result<studiocut_ai::ai_gaps::ReframePlan, String> {
    let project = analysis_snapshot(&state, project_id, revision)?;
    tauri::async_runtime::spawn_blocking(move || {
        studiocut_ai::ai_gaps::reframe_motion_clip(&project, clip_id, &target, at_seconds)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Face-tracked reframe plan for a portrait/square target: the crop
/// centre follows the one face detected consistently across the clip
/// (see ai_gaps::reframe_face_track_clip). When no face is found
/// consistently, the plan falls back to the motion-weighted reframe
/// and says so.
#[tauri::command]
async fn reframe_face_track(
    state: State<'_, Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
    clip_id: uuid::Uuid,
    target: String,
) -> Result<studiocut_ai::ai_gaps::ReframePlan, String> {
    let project = analysis_snapshot(&state, project_id, revision)?;
    tauri::async_runtime::spawn_blocking(move || {
        studiocut_ai::ai_gaps::reframe_face_track_clip(&project, clip_id, &target)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Whether local whisper.cpp exposes speaker segmentation, and which
/// diarization method StudioCut AI uses.
#[tauri::command]
fn diarization_capability() -> studiocut_ai::ai_gaps::DiarizationCapability {
    studiocut_ai::ai_gaps::diarization_capability()
}

/// Whether face tracking is available for auto-reframe, and which
/// reframe method StudioCut AI uses.
#[tauri::command]
fn reframe_capability() -> studiocut_ai::ai_gaps::ReframeCapability {
    studiocut_ai::ai_gaps::reframe_capability()
}

/// Auto rough cut: merges silence, filler and low-energy cuts the
/// user already ran, protecting the hook (scene scores pick the hook;
/// see analysis::rough_cut). Pure combination — it does not re-run
/// transcription or silence.
#[tauri::command]
fn rough_cut(
    state: State<Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
    silence: Vec<studiocut_ai::analysis::Cut>,
    fillers: Vec<studiocut_ai::analysis::Cut>,
    low_energy: Vec<studiocut_ai::analysis::Cut>,
    scenes: Vec<studiocut_ai::analysis::SceneCut>,
) -> Result<studiocut_ai::analysis::RoughCut, String> {
    let project = analysis_snapshot(&state, project_id, revision)?;
    let total = silence.len() + fillers.len() + low_energy.len() + scenes.len();
    if total > 2000
        || silence
            .iter()
            .chain(&fillers)
            .chain(&low_energy)
            .any(|c| c.start >= c.end)
        || scenes.iter().any(|s| s.start >= s.end)
    {
        return Err("Rough-cut inputs must be valid ranges (at most 2000 total)".into());
    }
    Ok(studiocut_ai::analysis::rough_cut(
        &silence,
        &fillers,
        &low_energy,
        &scenes,
        project.settings.fps,
    ))
}

/// Move one media asset into a named bin ("" = Unsorted). Bins live in
/// the project file only; a reimport/relink resets the bin.
#[tauri::command]
fn set_asset_bin(
    state: State<Workspace>,
    asset_id: uuid::Uuid,
    bin: String,
) -> Result<Project, String> {
    let bin = bin.trim().to_string();
    if bin.contains('\0') || bin.encode_utf16().count() > 80 {
        return Err("Bin name must be 1–80 characters".into());
    }
    let mut current = state.0.lock().map_err(|e| e.to_string())?;
    let project = current.as_ref().ok_or("No project is open")?;
    let mut next = project.clone();
    let asset = next
        .assets
        .iter_mut()
        .find(|a| a.id == asset_id)
        .ok_or("Media not found")?;
    asset.bin = bin;
    next.revision = next.revision.checked_add(1).ok_or("Revision overflow")?;
    next.validate()?;
    *current = Some(next.clone());
    Ok(next)
}

#[tauri::command]
async fn transcription_status(binary: String, model: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        studiocut_ai::transcription::local_available(&binary, &model)?;
        Ok("Local transcription ready".into())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn list_transcription_models(
    request: studiocut_ai::transcription::ModelsRequest,
) -> Result<Vec<studiocut_ai::transcription::ListedModel>, String> {
    tauri::async_runtime::spawn_blocking(move || studiocut_ai::transcription::list_models(&request))
        .await
        .map_err(|_| "Model list task failed".to_string())?
}

#[tauri::command]
async fn transcribe_clip(
    state: State<'_, Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
    clip_id: uuid::Uuid,
    options: studiocut_ai::transcription::TranscriptionOptions,
) -> Result<studiocut_ai::transcription::Transcript, String> {
    let project = analysis_snapshot(&state, project_id, revision)?;
    tauri::async_runtime::spawn_blocking(move || {
        studiocut_ai::transcription::transcribe(&project, clip_id, options)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn ripple_delete(
    state: State<Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
    cuts: Vec<studiocut_ai::analysis::Cut>,
) -> Result<Project, String> {
    let mut current = state.0.lock().map_err(|e| e.to_string())?;
    let project = current.as_ref().ok_or("No project is open")?;
    if project.id != project_id {
        return Err("Project changed; run analysis again".into());
    }
    let candidate = studiocut_ai::analysis::ripple(project, &cuts, revision)?;
    let next =
        state
            .1
            .lock()
            .map_err(|e| e.to_string())?
            .apply(project, candidate.tracks, revision)?;
    *current = Some(next.clone());
    Ok(next)
}

#[tauri::command]
fn add_captions(
    state: State<Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
    clip_id: uuid::Uuid,
    transcript: studiocut_ai::transcription::Transcript,
    preset: String,
) -> Result<Project, String> {
    let mut current = state.0.lock().map_err(|e| e.to_string())?;
    let project = current.as_ref().ok_or("No project is open")?;
    if project.id != project_id || project.revision != revision {
        return Err("Project changed; transcribe again".into());
    }
    let mut tracks = project.tracks.clone();
    tracks.push(studiocut_ai::transcription::caption_track(
        project,
        clip_id,
        &transcript,
        &preset,
    )?);
    let next = state
        .1
        .lock()
        .map_err(|e| e.to_string())?
        .apply(project, tracks, revision)?;
    *current = Some(next.clone());
    Ok(next)
}

#[tauri::command]
fn edit_timeline(
    state: State<Workspace>,
    project_id: uuid::Uuid,
    revision: u64,
    tracks: Option<Vec<studiocut_ai::timeline::Track>>,
    undo: Option<bool>,
) -> Result<Project, String> {
    let mut current = state.0.lock().map_err(|e| e.to_string())?;
    let project = current.as_ref().ok_or("No project is open")?;
    if project.id != project_id || project.revision != revision {
        return Err("Project changed; retry the edit".into());
    }
    let mut history = state.1.lock().map_err(|e| e.to_string())?;
    let next = match (tracks, undo) {
        (Some(tracks), None) => history.apply(project, tracks, revision)?,
        (None, Some(undo)) => history.travel(project, undo)?,
        _ => return Err("Invalid timeline edit".into()),
    };
    *current = Some(next.clone());
    Ok(next)
}

#[tauri::command]
fn set_markers(
    state: State<Workspace>,
    markers: Vec<studiocut_ai::Marker>,
) -> Result<Project, String> {
    let mut current = state.0.lock().map_err(|e| e.to_string())?;
    let project = current.as_ref().ok_or("No project is open")?;
    let mut next = project.clone();
    next.markers = markers;
    next.revision = next.revision.checked_add(1).ok_or("Revision overflow")?;
    next.validate()?;
    *current = Some(next.clone());
    Ok(next)
}

#[tauri::command]
fn rename_asset(
    state: State<Workspace>,
    asset_id: uuid::Uuid,
    name: String,
) -> Result<Project, String> {
    let name = name.trim().to_string();
    if name.is_empty() || name.encode_utf16().count() > 120 || name.contains('\0') {
        return Err("Asset name must be 1–120 characters".into());
    }
    let mut current = state.0.lock().map_err(|e| e.to_string())?;
    let project = current.as_ref().ok_or("No project is open")?;
    let mut next = project.clone();
    let asset = next
        .assets
        .iter_mut()
        .find(|a| a.id == asset_id)
        .ok_or("Media not found")?;
    asset.name = name;
    next.revision = next.revision.checked_add(1).ok_or("Revision overflow")?;
    next.validate()?;
    *current = Some(next.clone());
    Ok(next)
}

#[tauri::command]
fn remove_asset(state: State<Workspace>, asset_id: uuid::Uuid) -> Result<Project, String> {
    let mut current = state.0.lock().map_err(|e| e.to_string())?;
    let project = current.as_ref().ok_or("No project is open")?;
    if project
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .any(|c| c.asset_id == asset_id)
    {
        return Err("Remove the clips that use this media first".into());
    }
    let mut next = project.clone();
    let before = next.assets.len();
    next.assets.retain(|a| a.id != asset_id);
    if next.assets.len() == before {
        return Err("Media not found".into());
    }
    next.revision = next.revision.checked_add(1).ok_or("Revision overflow")?;
    next.validate()?;
    *current = Some(next.clone());
    Ok(next)
}

#[tauri::command]
async fn relink_asset(
    app: tauri::AppHandle,
    state: State<'_, Workspace>,
    asset_id: uuid::Uuid,
    path: String,
) -> Result<Project, String> {
    use tauri::Manager;
    let root = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?
        .join("media");
    let probed = tauri::async_runtime::spawn_blocking(move || {
        studiocut_ai::media::import_media(&root, std::path::Path::new(&path))
    })
    .await
    .map_err(|e| e.to_string())??;
    let mut current = state.0.lock().map_err(|e| e.to_string())?;
    let project = current.as_ref().ok_or("No project is open")?;
    let mut next = project.clone();
    let asset = next
        .assets
        .iter_mut()
        .find(|a| a.id == asset_id)
        .ok_or("Media not found")?;
    // Keep the original id (clips reference it) and import time; take
    // everything else from the fresh probe.
    let old_id = asset.id;
    let old_created = asset.created_at;
    *asset = probed;
    asset.id = old_id;
    asset.created_at = old_created;
    next.revision = next.revision.checked_add(1).ok_or("Revision overflow")?;
    next.validate()?;
    *current = Some(next.clone());
    Ok(next)
}

#[tauri::command]
async fn media_missing(state: State<'_, Workspace>) -> Result<Vec<uuid::Uuid>, String> {
    let assets = state
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .as_ref()
        .map(|p| p.assets.clone())
        .unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || {
        let mut missing = vec![];
        for asset in assets {
            if studiocut_ai::media::verified_source(&asset).is_err() {
                missing.push(asset.id);
            }
        }
        Ok(missing)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn playback_source(
    app: tauri::AppHandle,
    state: State<Workspace>,
    id: uuid::Uuid,
) -> Result<String, String> {
    use tauri::Manager;
    let current = state.0.lock().map_err(|e| e.to_string())?;
    let asset = current
        .as_ref()
        .and_then(|p| p.assets.iter().find(|a| a.id == id))
        .ok_or("Asset not in current project")?;
    let path = studiocut_ai::media::verified_source(asset)?;
    app.asset_protocol_scope()
        .allow_file(&path)
        .map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
async fn audio_chunk(
    state: State<'_, Workspace>,
    id: uuid::Uuid,
    start: f64,
    duration: f64,
) -> Result<Vec<u8>, String> {
    let asset = state
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .as_ref()
        .and_then(|p| p.assets.iter().find(|a| a.id == id))
        .cloned()
        .ok_or("Asset not in current project")?;
    tauri::async_runtime::spawn_blocking(move || {
        studiocut_ai::media::audio_chunk(&asset, start, duration)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn create_project(
    name: String,
    width: u32,
    height: u32,
    fps: u32,
    state: State<Workspace>,
) -> Result<Project, String> {
    let project = create(name, width, height, fps)?;
    *state.0.lock().map_err(|e| e.to_string())? = Some(project.clone());
    *state.1.lock().map_err(|e| e.to_string())? = Default::default();
    // A new project has no file yet: never autosave over a previous one.
    *state.2.lock().map_err(|e| e.to_string())? = None;
    Ok(project)
}

#[tauri::command]
async fn open_project(
    app: tauri::AppHandle,
    state: State<'_, Workspace>,
) -> Result<Option<OpenResult>, String> {
    let opened = tauri::async_runtime::spawn_blocking(
        move || -> Result<Option<(Project, std::path::PathBuf)>, String> {
            let Some(selected) = app
                .dialog()
                .file()
                .add_filter("StudioCut project", &["studiocut"])
                .blocking_pick_file()
            else {
                return Ok(None);
            };
            let path = selected.into_path().map_err(|e| e.to_string())?;
            load_project(&path).map(|p| Some((p, path)))
        },
    )
    .await
    .map_err(|e| e.to_string())??;
    if let Some((project, path)) = opened {
        *state.0.lock().map_err(|e| e.to_string())? = Some(project.clone());
        *state.1.lock().map_err(|e| e.to_string())? = Default::default();
        *state.2.lock().map_err(|e| e.to_string())? = Some(path.clone());
        return Ok(Some(OpenResult {
            project,
            path: path.to_string_lossy().into_owned(),
        }));
    }
    Ok(None)
}

#[tauri::command]
async fn open_project_path(state: State<'_, Workspace>, path: String) -> Result<Project, String> {
    let path = std::path::PathBuf::from(path);
    let project = {
        let load_path = path.clone();
        tauri::async_runtime::spawn_blocking(move || load_project(&load_path))
            .await
            .map_err(|e| e.to_string())??
    };
    *state.0.lock().map_err(|e| e.to_string())? = Some(project.clone());
    *state.1.lock().map_err(|e| e.to_string())? = Default::default();
    *state.2.lock().map_err(|e| e.to_string())? = Some(path);
    Ok(project)
}

#[tauri::command]
async fn save_project(
    app: tauri::AppHandle,
    state: State<'_, Workspace>,
) -> Result<Option<String>, String> {
    let project = state
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or("No project is open")?;
    let saved = tauri::async_runtime::spawn_blocking(move || -> Result<Option<String>, String> {
        let Some(selected) = app
            .dialog()
            .file()
            .add_filter("StudioCut project", &["studiocut"])
            .set_file_name("Untitled.studiocut")
            .blocking_save_file()
        else {
            return Ok(None);
        };
        let path = selected.into_path().map_err(|e| e.to_string())?;
        save(&project, &path)?;
        Ok(Some(path.to_string_lossy().into_owned()))
    })
    .await
    .map_err(|e| e.to_string())??;
    if let Some(ref path) = saved {
        // Remember the file for autosave; a successful manual save makes
        // any crash-recovery backup obsolete.
        *state.2.lock().map_err(|e| e.to_string())? = Some(std::path::PathBuf::from(path));
        let _ = std::fs::remove_file(studiocut_ai::autosave_path(std::path::Path::new(path)));
    }
    Ok(saved)
}

/// Silent save to the remembered path. Returns `None` when the project has
/// never been saved (frontend falls back to the dialog save). Writes the
/// crash-recovery backup first, then the real file.
#[tauri::command]
async fn save_project_silent(state: State<'_, Workspace>) -> Result<Option<String>, String> {
    let (project, path) = {
        let project = state
            .0
            .lock()
            .map_err(|e| e.to_string())?
            .clone()
            .ok_or("No project is open")?;
        let path = state.2.lock().map_err(|e| e.to_string())?.clone();
        (project, path)
    };
    let Some(path) = path else {
        return Ok(None);
    };
    let saved = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let backup = studiocut_ai::autosave_path(&path);
        let _ = save(&project, &backup);
        save(&project, &path)?;
        let _ = std::fs::remove_file(&backup);
        Ok(path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(Some(saved))
}

#[tauri::command]
fn check_recovery(path: String) -> Result<Option<RecoveryInfo>, String> {
    let path = std::path::PathBuf::from(path);
    let backup = studiocut_ai::autosave_path(&path);
    let (Ok(backup_meta), Ok(project_meta)) =
        (std::fs::metadata(&backup), std::fs::metadata(&path))
    else {
        return Ok(None);
    };
    let (Ok(backup_mtime), Ok(project_mtime)) = (backup_meta.modified(), project_meta.modified())
    else {
        return Ok(None);
    };
    let to_ms = |t: std::time::SystemTime| {
        t.duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    };
    if to_ms(backup_mtime) > to_ms(project_mtime) {
        Ok(Some(RecoveryInfo {
            backup_path: backup.to_string_lossy().into_owned(),
            backup_modified_ms: to_ms(backup_mtime),
            project_modified_ms: to_ms(project_mtime),
        }))
    } else {
        Ok(None)
    }
}

#[tauri::command]
async fn recover_project(state: State<'_, Workspace>, path: String) -> Result<Project, String> {
    let path = std::path::PathBuf::from(path);
    let backup = studiocut_ai::autosave_path(&path);
    let project = tauri::async_runtime::spawn_blocking(move || load_project(&backup))
        .await
        .map_err(|e| e.to_string())??;
    *state.0.lock().map_err(|e| e.to_string())? = Some(project.clone());
    *state.1.lock().map_err(|e| e.to_string())? = Default::default();
    // Remember the ORIGINAL path: the next save overwrites it, and the
    // backup is deleted then.
    *state.2.lock().map_err(|e| e.to_string())? = Some(path);
    Ok(project)
}

#[tauri::command]
fn discard_recovery(path: String) -> Result<(), String> {
    let backup = studiocut_ai::autosave_path(std::path::Path::new(&path));
    if backup.exists() {
        std::fs::remove_file(&backup).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SetupStatus {
    ffmpeg: bool,
    ffprobe: bool,
    whisper_cli: bool,
    whisper_model: bool,
    detail: String,
}

fn tool_runs(command: &str) -> bool {
    std::process::Command::new(command)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// First-run check: are the external tools the editor needs actually
/// installed? Uses the lib's resolution (bundled sidecar first, then
/// FFMPEG_PATH/FFPROBE_PATH, otherwise the bare name on PATH). No keys,
/// no network, no downloads.
#[tauri::command]
async fn setup_status(binary: String, model: String) -> Result<SetupStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let ffmpeg = tool_runs(&studiocut_ai::media::ffmpeg_path());
        let ffprobe = tool_runs(&studiocut_ai::media::ffprobe_path());
        let whisper_cli = !binary.trim().is_empty() && tool_runs(binary.trim());
        let whisper_model =
            !model.trim().is_empty() && std::path::Path::new(model.trim()).is_file();
        let mut missing: Vec<&str> = vec![];
        if !ffmpeg {
            missing.push("FFmpeg");
        }
        if !ffprobe {
            missing.push("FFprobe");
        }
        if !whisper_cli {
            missing.push("whisper-cli");
        }
        if !whisper_model {
            missing.push("whisper model file");
        }
        Ok(SetupStatus {
            ffmpeg,
            ffprobe,
            whisper_cli,
            whisper_model,
            detail: if missing.is_empty() {
                "All tools found.".into()
            } else {
                format!("Missing: {}.", missing.join(", "))
            },
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(serde::Serialize)]
struct ImportResult {
    project: Project,
    errors: Vec<String>,
}

#[tauri::command]
async fn import_media(
    app: tauri::AppHandle,
    state: State<'_, Workspace>,
    paths: Option<Vec<String>>,
) -> Result<Option<ImportResult>, String> {
    use tauri::Manager;
    let original = state
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or("Create or open a project first")?;
    let root = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?
        .join("media");
    let snapshot = original.clone();
    let result =
        tauri::async_runtime::spawn_blocking(move || -> Result<Option<ImportResult>, String> {
            let paths = match paths {
                Some(paths) => paths.into_iter().map(std::path::PathBuf::from).collect(),
                None => {
                    let Some(files) = app
                        .dialog()
                        .file()
                        .set_title("Import local media")
                        .blocking_pick_files()
                    else {
                        return Ok(None);
                    };
                    files
                        .into_iter()
                        .map(|p| p.into_path().map_err(|e| e.to_string()))
                        .collect::<Result<Vec<_>, _>>()?
                }
            };
            if paths.is_empty() || paths.len() > 100 {
                return Err("Select 1–100 media files per import".into());
            }
            let mut project = snapshot;
            let mut errors = Vec::new();
            for path in paths {
                match studiocut_ai::media::import_media(&root, &path) {
                    Ok(asset) if !project.assets.iter().any(|a| a.id == asset.id) => {
                        let mut next = project.clone();
                        next.schema_version = next.schema_version.max(2);
                        next.revision += 1;
                        next.assets.push(asset);
                        next.validate()?;
                        if serde_json::to_vec_pretty(&next)
                            .map_err(|e| e.to_string())?
                            .len() as u64
                            > studiocut_ai::MAX_PROJECT_BYTES
                        {
                            errors.push(format!("{}: project would exceed 1 MB", path.display()));
                        } else {
                            project = next;
                        }
                    }
                    Ok(_) => {}
                    Err(error) => errors.push(format!("{}: {error}", path.display())),
                }
            }
            Ok(Some(ImportResult { project, errors }))
        })
        .await
        .map_err(|e| e.to_string())??;
    if let Some(ref result) = result {
        let mut current = state.0.lock().map_err(|e| e.to_string())?;
        if current.as_ref() != Some(&original) {
            return Err("Project changed during import; please retry".into());
        }
        *current = Some(result.project.clone());
    }
    Ok(result)
}

#[tauri::command]
async fn media_preview(
    app: tauri::AppHandle,
    state: State<'_, Workspace>,
    id: uuid::Uuid,
) -> Result<studiocut_ai::media::Preview, String> {
    use tauri::Manager;
    let asset = state
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .as_ref()
        .and_then(|p| p.assets.iter().find(|a| a.id == id))
        .cloned()
        .ok_or("Asset not in current project")?;
    let root = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?
        .join("media");
    tauri::async_runtime::spawn_blocking(move || studiocut_ai::media::media_preview(&root, &asset))
        .await
        .map_err(|e| e.to_string())?
}

/// Hardware-encoder status for preview proxy work: which encoder
/// proxy generation would use for the saved preference, and whether
/// it is a hardware encoder. Advisory only — it never fails.
#[tauri::command]
fn preview_encoder_status(preference: String) -> studiocut_ai::media::EncoderSelection {
    studiocut_ai::media::preview_encoder_status(&preference)
}

#[tauri::command]
fn export_start(
    app: tauri::AppHandle,
    state: State<Workspace>,
    exports: State<studiocut_ai::render::ExportManager>,
    project_id: uuid::Uuid,
    revision: u64,
    output: String,
    settings: studiocut_ai::filtergraph::ExportSettings,
) -> Result<studiocut_ai::render::Snapshot, String> {
    license_gate(&app)?;
    use tauri::Manager;
    let project = state
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or("No project is open")?;
    if project.id != project_id || project.revision != revision {
        return Err("Project changed; reopen export".into());
    }
    let root = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?
        .join("media");
    exports.start_with_media_root(project, output.into(), settings, Some(root))
}
#[tauri::command]
fn export_status(
    exports: State<studiocut_ai::render::ExportManager>,
    id: Option<uuid::Uuid>,
    after: Option<u64>,
) -> Result<Option<studiocut_ai::render::Snapshot>, String> {
    exports.status(id, after)
}
#[tauri::command]
fn export_progress(
    exports: State<studiocut_ai::render::ExportManager>,
    id: Option<uuid::Uuid>,
    after: Option<u64>,
) -> Result<Option<studiocut_ai::progress_channel::ProgressBatch>, String> {
    exports.progress(id, after)
}
#[tauri::command]
fn export_cancel(
    exports: State<studiocut_ai::render::ExportManager>,
    id: uuid::Uuid,
) -> Result<studiocut_ai::render::Snapshot, String> {
    exports.cancel(id)
}
#[tauri::command]
fn lut_validate(text: String) -> Result<u32, String> {
    studiocut_ai::finishing::validate_cube(&text)
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(Workspace::default())
        .manage(AiState::default())
        .manage(studiocut_ai::render::ExportManager::default())
        .manage(studiocut_ai::mcp::McpCredentials::default())
        .invoke_handler(tauri::generate_handler![
            export_start,
            export_status,
            export_progress,
            export_cancel,
            lut_validate,
            director_context,
            director_credential,
            director_propose,
            measure_loudness,
            list_director_models,
            extract_thumbnail,
            director_vision_preview,
            director_apply,
            create_project,
            open_project,
            open_project_path,
            save_project,
            save_project_silent,
            check_recovery,
            recover_project,
            discard_recovery,
            setup_status,
            import_media,
            media_preview,
            media_missing,
            rename_asset,
            remove_asset,
            relink_asset,
            set_markers,
            edit_timeline,
            playback_source,
            audio_chunk,
            detect_silence,
            detect_scenes,
            detect_filler_cuts,
            rough_cut,
            set_asset_bin,
            ripple_delete,
            transcription_status,
            list_transcription_models,
            transcribe_clip,
            add_captions,
            license_status,
            license_activate,
            license_deactivate,
            analyze_scopes,
            proxy_status,
            generate_proxy,
            delete_proxy,
            proxy_playback_source,
            preview_encoder_status,
            detect_speakers,
            detect_beats,
            reframe_clip,
            reframe_motion_clip,
            reframe_face_track,
            diarization_capability,
            reframe_capability,
            studiocut_ai::mcp::mcp_list_presets,
            studiocut_ai::mcp::mcp_higgsfield_models,
            studiocut_ai::mcp::mcp_set_credential,
            studiocut_ai::mcp::mcp_test_connection,
            studiocut_ai::mcp::mcp_list_tools,
            studiocut_ai::mcp::mcp_call_tool,
            studiocut_ai::mcp::mcp_higgsfield_submit,
            studiocut_ai::mcp::mcp_higgsfield_status,
            studiocut_ai::mcp::mcp_higgsfield_cancel,
            studiocut_ai::mcp::mcp_download_result,
            studiocut_ai::mcp::mcp_tool_form_fields,
            studiocut_ai::mcp::mcp_validate_tool_arguments,
            studiocut_ai::mcp::mcp_oauth_discover_higgsfield,
            studiocut_ai::mcp::mcp_oauth_register_client,
            studiocut_ai::mcp::mcp_oauth_pkce_pair,
            studiocut_ai::mcp::mcp_oauth_authorization_url,
            studiocut_ai::mcp::mcp_oauth_exchange_code,
            studiocut_ai::mcp::mcp_oauth_refresh_token
        ])
        .run(tauri::generate_context!())
        .expect("Failed to launch StudioCut AI");
}
