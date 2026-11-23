CREATE TABLE assets (
    id TEXT PRIMARY KEY NOT NULL,
    source_path TEXT NOT NULL,
    name TEXT NOT NULL,
    size_bytes INTEGER NOT NULL CHECK (size_bytes > 0),
    modified_ns TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('video', 'audio', 'image')),
    duration REAL CHECK (duration > 0 AND duration <= 21600),
    width INTEGER CHECK (width > 0),
    height INTEGER CHECK (height > 0),
    fps REAL CHECK (fps >= 0),
    video_stream INTEGER CHECK (video_stream >= 0),
    audio_stream INTEGER CHECK (audio_stream >= 0),
    video_codec TEXT,
    audio_codec TEXT,
    sample_rate INTEGER CHECK (sample_rate > 0),
    channels INTEGER CHECK (channels > 0),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    CHECK (video_stream IS NOT NULL OR audio_stream IS NOT NULL)
);
CREATE UNIQUE INDEX assets_source_version ON assets(source_path, size_bytes, modified_ns);
PRAGMA user_version = 1;
