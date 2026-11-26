use serde_json::{json, Value};
use std::{fs, process::Command};
use studiocut_ai::{
    create_project,
    credentials::{Credentials, Provider},
    director::{self, Config, Director},
    edl::{self, Edl},
    load_project, media, save_project,
    timeline::{Clip, History, Track},
    Project,
};
use uuid::Uuid;
fn fixture() -> (tempfile::TempDir, Project) {
    let temp = tempfile::tempdir().unwrap();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/preview-fixture.mp4");
    let asset = media::import_media(&temp.path().join("cache"), &path).unwrap();
    let mut p = create_project("Director acceptance".into(), 1920, 1080, 30).unwrap();
    p.schema_version = 5;
    p.assets.push(asset.clone());
    let link = Uuid::new_v4();
    for kind in ["video", "audio"] {
        p.tracks.push(Track {
            id: Uuid::new_v4(),
            name: kind.into(),
            kind: kind.into(),
            muted: false,
            solo: false,
            locked: false,
            clips: vec![Clip {
                id: Uuid::new_v4(),
                asset_id: asset.id,
                link_id: Some(link),
                start: 30,
                source_in: 30,
                duration: 120,
                caption: None,
                effects: None,
            }],
        });
    }
    p.validate().unwrap();
    (temp, p)
}
fn edl(p: &Project, ops: Value) -> Edl {
    serde_json::from_value(json!({"version":1,"projectId":p.id,"baseRevision":p.revision,"summary":"Test edit proposal","operations":ops})).unwrap()
}
#[test]
fn atomic_approval_undo_persistence_and_stale_rejection() {
    let (temp, p) = fixture();
    let id = p.tracks[0].clips[0].id;
    let plan = edl(
        &p,
        json!([{"type":"move","clipId":id,"start":60},{"type":"zoom","clipId":id,"keys":[{"frame":30,"scale":1.2,"x":0.5,"y":0.5}]},{"type":"lut","clipId":id,"preset":"warm"}]),
    );
    let mut history = History::default();
    let next = edl::apply(&p, &plan, &mut history).unwrap();
    assert_eq!(next.revision, p.revision + 1);
    assert_eq!(next.tracks[1].clips[0].start, 60);
    assert!(edl::apply(&next, &plan, &mut history).is_err());
    let path = temp.path().join("project.studiocut");
    save_project(&next, &path).unwrap();
    assert_eq!(load_project(&path).unwrap(), next);
    let undone = history.travel(&next, true).unwrap();
    assert_eq!(undone.tracks, p.tracks);
    assert!(history.travel(&undone, true).is_err());
    assert_eq!(history.travel(&undone, false).unwrap().tracks, next.tracks);
    let bad = edl(
        &p,
        json!([{"type":"move","clipId":id,"start":80},{"type":"delete","clipId":Uuid::new_v4()}]),
    );
    assert!(edl::apply(&p, &bad, &mut History::default()).is_err());
    assert_eq!(p.tracks[0].clips[0].start, 30);
}
#[test]
fn edl_golden_contracts_and_typescript_parity() {
    let (temp, p) = fixture();
    let id = p.tracks[0].clips[0].id;
    let audio = p.tracks[1].clips[0].id;
    let split = json!({"type":"split","clipId":id,"frame":90,"rightIds":[Uuid::new_v4(),Uuid::new_v4()],"rightLinkId":Uuid::new_v4()});
    let caption_track = Uuid::new_v4();
    let caption =
        json!({"preset":"neon","timing":"word","words":[{"text":"Hello","start":30,"end":60}]});
    let clip = json!({"id":Uuid::new_v4(),"assetId":p.assets[0].id,"linkId":null,"start":180,"sourceIn":0,"duration":30});
    let mut cases: Vec<(Vec<Value>, bool)> = vec![
        (vec![json!({"type":"move","clipId":id,"start":60})], true),
        (
            vec![json!({"type":"trim","clipId":id,"start":60,"sourceIn":60,"duration":30})],
            true,
        ),
        (vec![split], true),
        (vec![json!({"type":"delete","clipId":id})], true),
        (vec![json!({"type":"ripple","start":60,"end":90})], true),
        (vec![json!({"type":"jCut","clipId":id,"frames":15})], true),
        (vec![json!({"type":"lCut","clipId":id,"frames":15})], true),
        (
            vec![
                json!({"type":"zoom","clipId":id,"keys":[{"frame":30,"scale":2.0,"x":0.5,"y":0.5}]}),
            ],
            true,
        ),
        (
            vec![json!({"type":"lut","clipId":id,"preset":"cool"})],
            true,
        ),
        (
            vec![
                json!({"type":"duck","clipId":audio,"keys":[{"frame":30,"gain":0.2},{"frame":90,"gain":1.0}]}),
            ],
            true,
        ),
        (
            vec![json!({"type":"broll","trackId":p.tracks[0].id,"clip":clip})],
            true,
        ),
        (
            vec![json!({"type":"sfx","trackId":p.tracks[1].id,"clip":clip})],
            true,
        ),
        (
            vec![json!({"type":"insert","trackId":p.tracks[0].id,"clip":clip})],
            true,
        ),
        (
            vec![
                json!({"type":"addTrack","trackId":caption_track,"name":"Captions","kind":"caption"}),
                json!({"type":"captions","trackId":caption_track,"clip":{"id":Uuid::new_v4(),"assetId":p.assets[0].id,"linkId":null,"start":30,"sourceIn":30,"duration":30,"caption":caption}}),
            ],
            true,
        ),
        (
            vec![json!({"type":"move","clipId":Uuid::new_v4(),"start":30})],
            false,
        ),
        (
            vec![json!({"type":"trim","clipId":id,"start":0,"sourceIn":239,"duration":60})],
            false,
        ),
        (vec![json!({"type":"jCut","clipId":id,"frames":31})], false),
        (vec![json!({"type":"ripple","start":100,"end":90})], false),
        (vec![json!({"type":"zoom","clipId":audio,"keys":[]})], false),
        (vec![json!({"type":"duck","clipId":id,"keys":[]})], false),
        (
            vec![
                json!({"type":"zoom","clipId":id,"keys":[{"frame":30,"scale":4.1,"x":0.5,"y":0.5}]}),
            ],
            false,
        ),
        (
            vec![json!({"type":"duck","clipId":audio,"keys":[{"frame":30,"gain":-0.1}]})],
            false,
        ),
        (vec![json!({"type":"move","clipId":id,"start":-1})], false),
        (vec![json!({"type":"move","clipId":id,"start":0.5})], false),
        (
            vec![json!({"type":"move","clipId":id,"start":10,"secret":"not allowed"})],
            false,
        ),
        (vec![json!({"type":"execute","command":"shell"})], false),
    ];
    let mut exported = Vec::new();
    for (ops, expected) in cases.drain(..) {
        let value = json!({"version":1,"projectId":p.id,"baseRevision":p.revision,"summary":"Golden contract","operations":ops});
        let result = edl::parse(&value.to_string()).and_then(|e| edl::compile(&p, &e));
        assert_eq!(result.is_ok(), expected, "{value}: {result:?}");
        exported.push(json!({"edl":value,"valid":expected}));
    }
    let mut locked = p.clone();
    locked.tracks[1].locked = true;
    for kind in ["move", "delete", "jCut", "lCut"] {
        let op = match kind {
            "move" => json!({"type":kind,"clipId":id,"start":60}),
            "delete" => json!({"type":kind,"clipId":id}),
            _ => json!({"type":kind,"clipId":id,"frames":1}),
        };
        assert!(edl::compile(&locked, &edl(&locked, json!([op]))).is_err());
    }
    let input = temp.path().join("cases.json");
    fs::write(
        &input,
        serde_json::to_vec(&json!({"project":p,"cases":exported})).unwrap(),
    )
    .unwrap();
    let status = Command::new("node")
        .arg("tests/director.mjs")
        .arg(&input)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .unwrap();
    assert!(status.success());
}
#[test]
fn provider_contracts_context_limits_and_missing_credentials() {
    let (_temp, mut p) = fixture();
    for i in 0..120 {
        let mut a = p.assets[0].clone();
        a.id = Uuid::new_v4();
        a.name = format!("asset-{i}");
        p.assets.push(a);
    }
    let context = director::context(&p);
    assert_eq!(context.omitted_assets, 21);
    assert!(context.bytes < 96000);
    assert!(!context.data.to_string().contains("sourcePath"));
    for provider in [
        Provider::Openai,
        Provider::Claude,
        Provider::Gemini,
        Provider::Groq,
        Provider::Openrouter,
        Provider::Compatible,
        Provider::Ollama,
        Provider::Bedrock,
        Provider::Vertex,
    ] {
        let c = Config {
            provider,
            model: "test-model".into(),
            endpoint: "https://example.com/v1/chat/completions".into(),
            max_tokens: 512,
        };
        let body = director::request_body(&c, "Test prompt");
        assert!(body.to_string().contains("Test prompt"));
        assert!(body.to_string().contains("baseRevision"));
        let response = match provider {
            Provider::Claude => json!({"content":[{"text":"{}"}]}),
            Provider::Gemini | Provider::Vertex => {
                json!({"candidates":[{"content":{"parts":[{"text":"{}"}]}}]})
            }
            Provider::Bedrock => json!({"output":{"message":{"content":[{"text":"{}"}]}}}),
            Provider::Ollama => json!({"message":{"content":"{}"}}),
            _ => json!({"choices":[{"message":{"content":"{}"}}]}),
        };
        assert_eq!(director::extract(provider, &response).unwrap(), "{}");
        assert!(director::extract(provider, &json!({})).is_err());
    }
    let config = Config {
        provider: Provider::Compatible,
        model: "test".into(),
        endpoint: "https://missing-credential.example/v1/chat/completions".into(),
        max_tokens: 512,
    };
    assert!(Director::default()
        .propose(
            &Credentials::default(),
            &p,
            config.clone(),
            "Edit".into(),
            true,
            "edit",
        )
        .err()
        .unwrap()
        .contains("credential"));
    assert!(Director::default()
        .propose(
            &Credentials::default(),
            &p,
            config.clone(),
            "Edit".into(),
            false,
            "edit",
        )
        .err()
        .unwrap()
        .contains("Approve"));
    for endpoint in [
        "http://example.com",
        "https://user:password@example.com",
        "https://example.com?key=secret",
    ] {
        let mut c = config.clone();
        c.endpoint = endpoint.into();
        assert!(c.url().is_err());
    }
    let mut local = config;
    local.provider = Provider::Ollama;
    local.endpoint = "http://127.0.0.1:11434/api/chat".into();
    assert!(local.url().is_ok());
    local.endpoint = "http://remote.example/api/chat".into();
    assert!(local.url().is_err());
}
