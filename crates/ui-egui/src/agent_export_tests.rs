//! Requirements Markdown export on both platforms: a save dialog on the desktop, a download in the
//! browser.
//!
//! The browser has no save dialog, so an export that always called `pick_save` did nothing there: the
//! click produced no file and no message. This pins the routing by platform capability.

use std::sync::{Arc, Mutex};

use figcraft_engine::Session;
use serde_json::json;

use crate::agent::Request;
use crate::{FigApp, NoServices, Services};

pub(crate) type Downloads = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// Services that save by downloads and keep them (`deliver` false: the browser refused).
struct Browser {
    got: Downloads,
    deliver: bool,
}

impl Services for Browser {
    fn downloads(&self) -> bool {
        true
    }
    fn download(&mut self, name: &str, bytes: &[u8]) -> bool {
        self.got.lock().unwrap().push((name.to_string(), bytes.to_vec()));
        self.deliver
    }
}

/// A session holding one requirements document with several sections and non-ASCII text.
fn session() -> (Session, String) {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    let created = s
        .execute(
            "requirements.upsert",
            &json!({
                "title": "企业微信侧边栏",
                "summary": "智能回复的侧边栏",
                "goals": ["一句话生成话术"],
                "requirements": [{"title": "话术卡", "detail": "显示候选话术", "priority": "must"}],
                "acceptanceCriteria": ["点击采用后填入输入框"],
            }),
        )
        .unwrap();
    let id = created["id"].as_str().expect("an id").to_string();
    (s, id)
}

fn app_with(s: Session, services: Box<dyn Services>) -> FigApp {
    let mut app = FigApp::new(s, services);
    app.ui.agent = Some(crate::agent::AgentPanel::new("doc-1"));
    app
}

#[test]
fn the_browser_downloads_the_whole_markdown_and_never_opens_a_save_dialog() {
    // The defect this pins: the export always called `pick_save`, which a browser cannot answer, so the
    // click did nothing at all.
    let (s, id) = session();
    let got: Downloads = Arc::new(Mutex::new(Vec::new()));
    let mut app = app_with(s, Box::new(Browser { got: Arc::clone(&got), deliver: true }));

    app.ui.agent_requests.push(Request::ExportRequirements { id });
    crate::agent::perform_requests(&mut app);

    let downloaded = got.lock().unwrap();
    assert_eq!(downloaded.len(), 1, "one download, not none and not two");
    let (name, bytes) = &downloaded[0];
    assert!(name.ends_with(".md"), "the name says what it is: {name}");
    assert!(name.contains("企业微信侧边栏"), "and it comes from the title: {name}");

    let text = String::from_utf8(bytes.clone()).expect("the Markdown is UTF-8");
    for expected in ["# 企业微信侧边栏", "智能回复的侧边栏", "一句话生成话术", "话术卡", "点击采用后填入输入框"] {
        assert!(text.contains(expected), "the whole document is in the download, `{expected}` missing");
    }
    assert!(text.lines().count() > 5, "a document, not a preview line");
}

#[test]
fn a_browser_that_refuses_the_download_reports_it() {
    // A refused download must say so: silence looks like success and the person looks for a file.
    let (s, id) = session();
    let got: Downloads = Arc::new(Mutex::new(Vec::new()));
    let mut app = app_with(s, Box::new(Browser { got: Arc::clone(&got), deliver: false }));

    app.ui.agent_requests.push(Request::ExportRequirements { id });
    crate::agent::perform_requests(&mut app);

    assert_eq!(got.lock().unwrap().len(), 1, "the download was attempted");
    let panel = app.ui.agent.as_ref().expect("panel");
    assert!(panel.error.is_some(), "a refusal is visible");
    assert!(panel.export_note.is_none(), "and it does not claim success");
}

#[test]
fn an_export_failure_leaves_the_document_alone() {
    // Exporting is not an edit: a failure must not touch the design or the undo history.
    let (s, id) = session();
    let got: Downloads = Arc::new(Mutex::new(Vec::new()));
    let mut app = app_with(s, Box::new(Browser { got, deliver: false }));
    let depth = app.session.undo_depth();
    let layers = app.session.doc().expect("doc").nodes().count();

    app.ui.agent_requests.push(Request::ExportRequirements { id });
    crate::agent::perform_requests(&mut app);

    assert_eq!(app.session.undo_depth(), depth, "no undo entry");
    assert_eq!(app.session.doc().expect("doc").nodes().count(), layers, "no document change");
}

#[test]
fn a_desktop_build_does_not_download() {
    // The desktop has save dialogs, so it must keep using one: a download would bypass the person's
    // choice of where the file goes.
    let (s, _id) = session();
    let app = app_with(s, Box::new(NoServices));
    assert!(!app.services.downloads(), "the default services have no downloads, which is the desktop's shape");
}
