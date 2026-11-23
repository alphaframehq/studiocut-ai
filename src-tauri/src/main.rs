#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Mutex;
use studiocut_ai::{create_project as create, load_project, save_project as save, Project};
use tauri::State;
use tauri_plugin_dialog::DialogExt;

#[derive(Default)]
struct Workspace(
    Mutex<Option<Project>>,
    Mutex<studiocut_ai::timeline::History>,
);

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
) -> Result<studiocut_ai::director::Proposal, String> {
    use tauri::Manager;
    license_gate(&app)?;
    let project = analysis_snapshot(&state, project_id, revision)?;
    tauri::async_runtime::spawn_blocking(move || {
        let ai = app.state::<AiState>();
        ai.director
            .propose(&ai.credentials, &project, config, instruction, consent)
    })
    .await
    .map_err(|_| "Director task failed".to_string())?
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
    Ok(project)
}

#[tauri::command]
async fn open_project(
    app: tauri::AppHandle,
    state: State<'_, Workspace>,
) -> Result<Option<Project>, String> {
    let project =
        tauri::async_runtime::spawn_blocking(move || -> Result<Option<Project>, String> {
            let Some(selected) = app
                .dialog()
                .file()
                .add_filter("StudioCut project", &["studiocut"])
                .blocking_pick_file()
            else {
                return Ok(None);
            };
            let path = selected.into_path().map_err(|e| e.to_string())?;
            load_project(&path).map(Some)
        })
        .await
        .map_err(|e| e.to_string())??;
    if let Some(ref project) = project {
        *state.0.lock().map_err(|e| e.to_string())? = Some(project.clone());
        *state.1.lock().map_err(|e| e.to_string())? = Default::default();
    }
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
    tauri::async_runtime::spawn_blocking(move || -> Result<Option<String>, String> {
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
    use tauri::Manager;
    license_gate(&app)?;
    let project = state
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or("No project is open")?;
    if project.id != project_id || project.revision != revision {
        return Err("Project changed; reopen export".into());
    }
    exports.start(project, output.into(), settings)
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
        .manage(Workspace::default())
        .manage(AiState::default())
        .manage(studiocut_ai::render::ExportManager::default())
        .invoke_handler(tauri::generate_handler![
            export_start,
            export_status,
            export_progress,
            export_cancel,
            lut_validate,
            director_context,
            director_credential,
            director_propose,
            director_apply,
            create_project,
            open_project,
            save_project,
            import_media,
            media_preview,
            edit_timeline,
            playback_source,
            audio_chunk,
            detect_silence,
            ripple_delete,
            transcription_status,
            transcribe_clip,
            add_captions,
            license_status,
            license_activate,
            license_deactivate
        ])
        .run(tauri::generate_context!())
        .expect("Failed to launch StudioCut AI");
}
