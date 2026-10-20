//! Pictures in notes, as agents meet them: `note read` brings a note's
//! pictures along when the model takes pictures (and only names them when
//! it doesn't), `note image` adds one from a file the agent may read.

mod common;

use base64::Engine as _;
use common::*;
use serde_json::{Value, json};
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

/// A harness whose model does (or doesn't) take pictures.
async fn harness_seeing(script: Vec<Reply>, vision: bool) -> Harness {
    let mock = Mock::start(script).await;
    let dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let work = work_dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let config = format!(
        "default_model = \"mock/m\"\n[agent]\nreview_interval = 0\n[tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n[[provider]]\nid = \"mock\"\nname = \"Mock\"\nkind = \"openai_chat\"\nbase_url = \"{}\"\ndiscover = false\n[[provider.models]]\nid = \"m\"\ncontext_window = 100000\nmax_output = 2000\nvision = {vision}\n",
        mock.url
    );
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let engine = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();
    Harness {
        engine,
        mock,
        dir,
        work,
        _work_dir: work_dir,
    }
}

/// A PNG's first bytes: enough to be taken for one.
fn png(salt: u8) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    b.extend(320u32.to_be_bytes());
    b.extend(200u32.to_be_bytes());
    b.extend([8, 6, 0, 0, 0, salt]);
    b
}

async fn picture_note(h: &Harness) -> (Note, String) {
    let data = base64::engine::general_purpose::STANDARD.encode(png(7));
    let a: NoteAsset = serde_json::from_value(
        h.engine
            .handle(
                Request::PutNoteAsset {
                    mime: "image/png".into(),
                    data,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let n: Note = serde_json::from_value(
        h.engine
            .handle(
                Request::SaveNote {
                    id: None,
                    title: "Receipts".into(),
                    content: format!(
                        "# Receipts\n\nThe hotel:\n\n![hotel receipt](assets/{})\n",
                        a.name
                    ),
                    base_version: None,
                    session_id: None,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    (n, a.name)
}

/// The image parts the model was sent (data URLs).
fn images_sent(req: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for m in req["messages"].as_array().into_iter().flatten() {
        for p in m["content"].as_array().into_iter().flatten() {
            if let Some(u) = p["image_url"]["url"].as_str() {
                out.push(u.to_string());
            }
        }
    }
    out
}

#[tokio::test]
async fn agents_see_a_notes_pictures_when_their_model_takes_pictures() {
    let h = harness_seeing(vec![], true).await;
    let (n, name) = picture_note(&h).await;
    h.mock.push(Reply::Tools(vec![(
        "note".into(),
        json!({"action": "read", "id": n.id}),
    )]));
    h.mock.push(Reply::Text("It is a hotel receipt.".into()));
    let s = h.session().await;
    h.send(&s.id, "What's in my receipts note?").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    let (_, err, text) = &results[0];
    assert!(
        !err && text.contains(&format!("assets/{name}")) && text.contains("Its pictures follow"),
        "{text}"
    );
    let reqs = h.mock.agent_requests();
    let sent = images_sent(reqs.last().unwrap());
    let want = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png(7))
    );
    assert_eq!(sent, vec![want], "the picture itself went to the model");
}

#[tokio::test]
async fn a_model_without_eyes_is_told_the_pictures_are_there() {
    let h = harness_seeing(vec![], false).await;
    let (n, _) = picture_note(&h).await;
    h.mock.push(Reply::Tools(vec![(
        "note".into(),
        json!({"action": "read", "id": n.id}),
    )]));
    h.mock.push(Reply::Text("ok".into()));
    let s = h.session().await;
    h.send(&s.id, "Read my receipts").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results[0].2.contains("doesn't see pictures"),
        "{}",
        results[0].2
    );
    assert!(images_sent(h.mock.agent_requests().last().unwrap()).is_empty());
}

#[tokio::test]
async fn an_agent_adds_a_picture_from_a_file() {
    let h = harness_seeing(vec![], true).await;
    let (n, first) = picture_note(&h).await;
    let s = h.session().await;
    std::fs::write(h.work.join("chart.png"), png(9)).unwrap();
    std::fs::write(h.work.join("notes.txt"), "not a picture").unwrap();
    h.mock.push(Reply::Tools(vec![
        (
            "note".into(),
            json!({"action": "image", "id": n.id, "path": "chart.png", "alt": "Spending [by] month"}),
        ),
        (
            "note".into(),
            json!({"action": "image", "path": "notes.txt"}),
        ),
    ]));
    h.mock.push(Reply::Text("Added.".into()));
    h.send(&s.id, "Put chart.png in my receipts note").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        !results[0].1 && results[0].2.contains("Added the picture"),
        "{:?}",
        results[0]
    );
    assert!(
        results[1].1 && results[1].2.contains("isn't a PNG"),
        "{:?}",
        results[1]
    );
    let after: Note = serde_json::from_value(
        h.engine
            .handle(Request::GetNote { id: n.id.clone() }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    let added = after.content.lines().last().unwrap().to_string();
    assert!(
        added.starts_with("![Spending by month](assets/")
            && added.ends_with(".png)")
            && !added.contains(&first),
        "{added}"
    );
    assert!(
        after.agent_edit && after.content.contains(&first),
        "what was there stays"
    );
}
