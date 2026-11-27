//! Build script for the StudioCut AI desktop app.
//!
//! The Tauri build step copies the `bundle.externalBin` sidecars (the
//! bundled FFmpeg/FFprobe binaries) next to the app executable. Those
//! sidecars are downloaded at release-build time by the release workflow
//! and are absent on a fresh checkout, so a plain `cargo build` or
//! `cargo test` would fail in the build script with
//! "resource path `binaries/ffmpeg-<triple>` doesn't exist".
//!
//! The sidecars are only required when bundling an installer (`tauri
//! build`, release profile). Development and test builds (debug profile)
//! tolerate their absence so the test suite can run without them; release
//! builds still fail loudly, so an installer is never shipped without the
//! bundled FFmpeg binaries.

fn main() {
    // Re-run this script if a sidecar appears later (for example, a
    // developer downloads the binaries after a first build without them).
    if let Ok(target) = std::env::var("TARGET") {
        for tool in ["ffmpeg", "ffprobe"] {
            println!("cargo:rerun-if-changed=binaries/{tool}-{target}");
        }
    }

    if let Err(error) = tauri_build::try_build(Default::default()) {
        let message = format!("{error:#}");
        let target = std::env::var("TARGET").unwrap_or_default();
        let sidecars_missing = ["ffmpeg", "ffprobe"].iter().any(|tool| {
            !std::path::Path::new("binaries")
                .join(format!("{tool}-{target}"))
                .exists()
        });
        let is_missing_sidecar_error =
            message.contains("doesn't exist") && message.contains("binaries/");
        let debug_build = std::env::var("PROFILE").as_deref() != Ok("release");
        if sidecars_missing && is_missing_sidecar_error && debug_build {
            println!(
                "cargo:warning=FFmpeg sidecar binaries are not present in src-tauri/binaries/; continuing without them. Release builds download them first (see .github/workflows/release.yml)."
            );
        } else {
            println!("{message}");
            std::process::exit(1);
        }
    }
}
