use std::fs;
use studiocut_ai::{create_project, load_project, save_project, MAX_PROJECT_BYTES};

#[test]
fn create_atomic_save_replace_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("edit with spaces.studiocut");
    let mut project = create_project("  First edit  ".into(), 1920, 1080, 30).unwrap();
    assert_eq!(project.name, "First edit");
    save_project(&project, &path).unwrap();
    assert_eq!(load_project(&path).unwrap(), project);
    let original = fs::File::open(&path).unwrap();
    project.name = "Second edit".into();
    // On Windows the open handle on the destination blocks the replace
    // (Rust opens files without FILE_SHARE_DELETE), so release it first.
    // Unix keeps the handle open to verify the old inode is untouched.
    #[cfg(not(unix))]
    drop(original);
    save_project(&project, &path).unwrap();
    assert_eq!(load_project(&path).unwrap(), project);
    #[cfg(unix)]
    {
        let old: studiocut_ai::Project = serde_json::from_reader(original).unwrap();
        assert_eq!(
            old.name, "First edit",
            "Replacement must not rewrite the original inode"
        );
    }
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn rejects_invalid_boundaries_and_preserves_existing_files() {
    for size in [15, 8193] {
        assert!(create_project("Edit".into(), size, 1080, 30).is_err());
    }
    for size in [16, 8192] {
        assert!(create_project("Edit".into(), size, size, 30).is_ok());
    }
    assert!(create_project(" ".into(), 1920, 1080, 30).is_err());
    assert!(create_project("a".repeat(121), 1920, 1080, 30).is_err());
    assert!(create_project("Edit".into(), 1920, 1080, 29).is_err());
    let directory = tempfile::tempdir().unwrap();
    let project = create_project("Edit".into(), 1920, 1080, 30).unwrap();
    for name in ["source.mp4", "not-a-project.studiocut"] {
        let path = directory.path().join(name);
        fs::write(&path, b"untouched").unwrap();
        assert!(save_project(&project, &path).is_err());
        assert_eq!(fs::read(path).unwrap(), b"untouched");
    }
    let path = directory.path().join("valid.studiocut");
    save_project(&project, &path).unwrap();
    let before = fs::read(&path).unwrap();
    let mut invalid = project;
    invalid.settings.width = 0;
    assert!(save_project(&invalid, &path).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(save_project(&invalid, &directory.path().join("missing/file.studiocut")).is_err());
    fs::write(&path, vec![b' '; MAX_PROJECT_BYTES as usize + 1]).unwrap();
    assert!(load_project(&path).unwrap_err().contains("exceeds"));
    fs::write(&path, b"{broken").unwrap();
    assert!(load_project(&path).is_err());
    assert!(load_project(&directory.path().join("missing.studiocut")).is_err());
}

#[test]
fn autosave_backup_round_trips_as_a_valid_project() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("edit.studiocut");
    let project = create_project("Backup".into(), 1920, 1080, 30).unwrap();
    save_project(&project, &path).unwrap();
    let backup = studiocut_ai::autosave_path(&path);
    save_project(&project, &backup).unwrap();
    assert_eq!(load_project(&backup).unwrap(), project);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
}
