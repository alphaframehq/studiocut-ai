pub mod agents;
pub mod ai_gaps;
pub mod analysis;
pub mod credentials;
pub mod director;
pub mod edl;
pub mod face;
pub mod filtergraph;
pub mod finishing;
pub mod license;
pub mod mcp;
pub mod media;
pub mod progress_channel;
pub mod proxy;
pub mod render;
pub mod scopes;
pub mod thumbnail;
pub mod timeline;
pub mod transcription;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub const MAX_PROJECT_BYTES: u64 = 1_048_576;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub audio_sample_rate: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Marker {
    pub id: Uuid,
    pub frame: u32,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Project {
    pub schema_version: u32,
    pub id: Uuid,
    pub name: String,
    pub revision: u64,
    pub created_at: u64,
    pub settings: Settings,
    pub assets: Vec<media::Asset>,
    pub tracks: Vec<timeline::Track>,
    /// Timeline markers. Optional so v1–v6 files load with an empty list.
    #[serde(default)]
    pub markers: Vec<Marker>,
}

impl Project {
    pub fn validate(&self) -> Result<(), String> {
        if ![1, 2, 3, 4, 5, 6].contains(&self.schema_version)
            || self.revision > 9_007_199_254_740_991
            || (self.schema_version == 1 && (self.revision != 0 || !self.assets.is_empty()))
        {
            return Err("Unsupported project version or revision".into());
        }
        if self.id.get_version_num() != 4 || self.created_at > 9_007_199_254_740_991 {
            return Err("Invalid project identity or creation date".into());
        }
        if self.name.trim().is_empty() || self.name.encode_utf16().count() > 120 {
            return Err("Project name must be 1–120 characters".into());
        }
        let s = &self.settings;
        if !(16..=8192).contains(&s.width)
            || !(16..=8192).contains(&s.height)
            || ![24, 25, 30, 50, 60].contains(&s.fps)
            || s.audio_sample_rate != 48000
        {
            return Err("Unsupported project settings".into());
        }
        timeline::validate(self)?;
        let mut ids = std::collections::HashSet::new();
        for asset in &self.assets {
            asset.validate()?;
            if !ids.insert(asset.id) {
                return Err("Duplicate project asset".into());
            }
        }
        if self.markers.len() > 200 {
            return Err("Maximum 200 timeline markers".into());
        }
        let mut marker_ids = std::collections::HashSet::new();
        let max_frame = 21600 * 60;
        for marker in &self.markers {
            if marker.id.get_version_num() != 4
                || !marker_ids.insert(marker.id)
                || marker.frame > max_frame
                || marker.label.trim().is_empty()
                || marker.label.encode_utf16().count() > 80
            {
                return Err("Invalid timeline marker".into());
            }
        }
        Ok(())
    }
}

pub fn create_project(name: String, width: u32, height: u32, fps: u32) -> Result<Project, String> {
    let project = Project {
        schema_version: 1,
        id: Uuid::new_v4(),
        name: name.trim().to_owned(),
        revision: 0,
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis() as u64,
        settings: Settings {
            width,
            height,
            fps,
            audio_sample_rate: 48000,
        },
        assets: vec![],
        tracks: vec![],
        markers: vec![],
    };
    project.validate()?;
    Ok(project)
}

pub fn load_project(path: &Path) -> Result<Project, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_PROJECT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_PROJECT_BYTES {
        return Err("Project file exceeds 1 MB".into());
    }
    let project: Project =
        serde_json::from_slice(&bytes).map_err(|e| format!("Invalid project: {e}"))?;
    project.validate()?;
    Ok(project)
}

/// Sibling crash-recovery backup: `My Cut.studiocut` ->
/// `My Cut.autosave.studiocut`. Same extension, so it stays a valid
/// project file that `load_project` can read back.
pub fn autosave_path(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("project");
    path.with_file_name(format!("{stem}.autosave.studiocut"))
}

pub fn save_project(project: &Project, path: &Path) -> Result<(), String> {
    project.validate()?;
    if path.extension().and_then(|s| s.to_str()) != Some("studiocut") {
        return Err("Use the .studiocut extension. Media files cannot be overwritten.".into());
    }
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err("Refusing to overwrite a symbolic link".into());
    }
    if path.exists() {
        load_project(path)
            .map_err(|_| "Refusing to overwrite a file that is not a valid StudioCut project")?;
    }
    let bytes = serde_json::to_vec_pretty(project).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_PROJECT_BYTES {
        return Err("Project file exceeds 1 MB".into());
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    temporary.write_all(&bytes).map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    temporary.persist(path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn autosave_path_is_a_valid_sibling_project_name() {
        let path = autosave_path(Path::new("/tmp/My Cut.studiocut"));
        assert_eq!(path, Path::new("/tmp/My Cut.autosave.studiocut"));
        assert_eq!(path.extension().and_then(|s| s.to_str()), Some("studiocut"));
    }
}
