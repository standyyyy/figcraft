#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Probe: does a turn's own work move the live revision?

use serde_json::json;

#[test]
#[ignore]
fn probe_turn_revision() {
    let prefs = figcraft_engine::prefs::AgentPrefs {
        enabled: true,
        base_url: "http://192.168.15.125:8111/v1".into(),
        model: "DeepSeek-V4.1-Flash-1017".into(),
        timeout_seconds: 300,
        ..Default::default()
    };
    let config = figcraft_agent::AgentConfig::from_prefs(&prefs).expect("cfg");

    // The live session, as the app holds it.
    let mut live = figcraft_engine::Session::new();
    live.execute("file.new", &json!({})).expect("file.new");
    let ds = live.ds().expect("doc");
    let (uid, revision) = (ds.uid, ds.revision);
    eprintln!("=== live before: uid={uid} revision={revision}");

    // The snapshot, exactly as `document_snapshot` builds it.
    let document = figcraft_format::to_json(live.ds().expect("doc").doc.as_ref()).expect("to_json");
    let shadow_session = figcraft_engine::Session::with_document(serde_json::from_value(document).expect("doc"));
    let shadow = figcraft_agent::shadow::Shadow::capture(&shadow_session).expect("capture");

    let t = figcraft_agent::runtime::Turn {
        turn: 1,
        request: "Create a rectangle 100x60 at 10,10".into(),
        document_uid: uid,
        base_revision: revision,
        config,
        limits: figcraft_agent::run::Limits::default(),
        cancel: figcraft_agent::run::CancelToken::new(),
        history: Vec::new(),
    };
    let terminal = figcraft_agent::runtime::run(t, shadow, &figcraft_agent::EnvironmentCredentials, &mut |_| {});

    let after = live.ds().expect("doc").revision;
    eprintln!("=== live after the turn: revision={after} (turn did not touch it)");
    eprintln!("=== terminal ok: {}", matches!(terminal, figcraft_agent::TurnTerminal::Success { .. }));
    if let figcraft_agent::TurnTerminal::Success { commit, .. } = &terminal {
        match commit {
            Some(c) => eprintln!("=== accepts live? {}", c.accepts(uid, after)),
            None => eprintln!("=== no envelope"),
        }
    }
}
