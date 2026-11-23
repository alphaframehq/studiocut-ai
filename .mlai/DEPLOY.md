# Native release qualification

- Phase 2 in PLAN.md awaits real Windows/macOS/Linux desktop evidence and human editorial-footage review. No next phase is defined.
- Run `npm run build`, Cargo check/build/test with `--locked --manifest-path src-tauri/Cargo.toml`, and `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`.
- 2026-11-22: full Rust suite passed all 30 tests; frontend/native builds and formatting passed. `src-tauri/tests/qualification.rs` has five real-media tests: independent grading, four caption preset references, mastering references, requested-encoder fallback and delayed audio content at 0.5x/1x/1.5x/2x.
- Delayed audio truncation was isolated to `adelay` feeding sidechain/mix paths with both voice and music delayed. Placement now uses `asetpts=N/SR/TB+start/TB,aresample=48000:first_pts=0`. Signal tests assert leading silence, both tones, tail content and duration. Normal-speed mastering reference RMSE was 0.000001 (limit 0.003).
- `render.rs::validate_output` validates audio/video stream durations within 0.15s, not merely container duration. A real malformed-export test verifies short audio rejection.
- Native qualification must record OS/architecture, FFmpeg build, GPU/driver, selected encoder, stream durations, WebView import/export/progress/reconnect/cancel, file dialogs and playback. Review real footage by eye/ear before release.
- This container has no GPU or desktop session. All requested hardware encoders used disclosed CPU fallback; this does not verify acceleration. Do not retry the known bwrap/glycin GTK picker sandbox failure here.
