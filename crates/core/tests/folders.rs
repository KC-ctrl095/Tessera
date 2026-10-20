//! "New folder" in the folder picker (`create_dir`): one plain-named folder inside an existing one.

mod common;

use common::*;
use tessera_core::Engine;
use tessera_proto::*;

async fn make(e: &Engine, parent: &str, name: &str) -> Result<DirEntry, ApiError> {
    let r = e
        .handle(
            Request::CreateDir {
                parent: parent.into(),
                name: name.into(),
            },
            Some("test-device"),
        )
        .await?;
    Ok(serde_json::from_value(r).unwrap())
}

fn refused(r: Result<DirEntry, ApiError>) -> ApiError {
    r.expect_err("should have been refused")
}

#[tokio::test]
async fn makes_one_folder_and_shows_in_the_listing() {
    let h = harness(vec![], 100_000).await;
    let parent = h.work.display().to_string();
    let made = make(&h.engine, &parent, "Ada's notes").await.unwrap();
    assert!(made.is_dir);
    assert_eq!(made.name, "Ada's notes");
    assert_eq!(made.path, h.work.join("Ada's notes").display().to_string());
    assert!(h.work.join("Ada's notes").is_dir());
    // The name is trimmed, and the listing request sees the new folder.
    let made = make(&h.engine, &parent, "  Bob  ").await.unwrap();
    assert_eq!(made.name, "Bob");
    let list: Vec<DirEntry> = serde_json::from_value(
        h.engine
            .handle(Request::ListDir { path: parent }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    let names: Vec<_> = list.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["Ada's notes", "Bob"]);
}

#[tokio::test]
async fn names_that_are_paths_or_odd_are_refused() {
    let h = harness(vec![], 100_000).await;
    let parent = h.work.display().to_string();
    let long = "x".repeat(256);
    for bad in [
        "",
        "   ",
        ".",
        "..",
        "a/b",
        "/etc",
        "../escape",
        "a\\b",
        "tab\there",
        "new\nline",
        "nul\0byte",
        "esc\u{1b}[31m",
        "flip\u{202E}gnp",
        "zero\u{200B}width",
        &long,
    ] {
        let e = refused(make(&h.engine, &parent, bad).await);
        assert_eq!(e.code, ErrorCode::BadRequest, "{bad:?}");
        assert!(!e.message.is_empty());
    }
    // Nothing was made, in the folder or beside it.
    assert_eq!(std::fs::read_dir(&h.work).unwrap().count(), 0);
    assert!(!h.work.parent().unwrap().join("escape").exists());
    // A long name that fits is fine, and so are dots inside a name.
    make(&h.engine, &parent, &"y".repeat(255)).await.unwrap();
    make(&h.engine, &parent, "v1.2.beta").await.unwrap();
    make(&h.engine, &parent, ".hidden").await.unwrap();
}

#[tokio::test]
async fn parents_must_be_existing_folders_given_in_full() {
    let h = harness(vec![], 100_000).await;
    let parent = h.work.display().to_string();
    std::fs::write(h.work.join("a file"), "x").unwrap();
    for bad in [
        "",
        "relative/dir",
        ".",
        &format!("{parent}/missing"),
        &format!("{parent}/a file"),
        "nul\0",
    ] {
        let e = refused(make(&h.engine, bad, "new").await);
        assert_eq!(e.code, ErrorCode::BadRequest, "{bad:?}");
    }
}

#[tokio::test]
async fn something_already_there_is_refused_and_left_alone() {
    let h = harness(vec![], 100_000).await;
    let parent = h.work.display().to_string();
    make(&h.engine, &parent, "Ada").await.unwrap();
    std::fs::write(h.work.join("Ada").join("keep.txt"), "mine").unwrap();
    let e = refused(make(&h.engine, &parent, "Ada").await);
    assert_eq!(e.code, ErrorCode::Conflict);
    assert!(
        e.message.starts_with("A folder called Ada"),
        "{}",
        e.message
    );
    assert_eq!(
        std::fs::read_to_string(h.work.join("Ada").join("keep.txt")).unwrap(),
        "mine"
    );
    std::fs::write(h.work.join("notes.txt"), "x").unwrap();
    let e = refused(make(&h.engine, &parent, "notes.txt").await);
    assert_eq!(e.code, ErrorCode::Conflict);
    assert!(e.message.starts_with("A file called"), "{}", e.message);
}

#[cfg(unix)]
#[tokio::test]
async fn a_link_of_that_name_is_never_followed() {
    let h = harness(vec![], 100_000).await;
    let parent = h.work.display().to_string();
    let elsewhere = tempfile::tempdir().unwrap();
    // To a folder that exists, and to one that doesn't (a dangling link: creating "through" it
    // would make the folder at the link's far end).
    std::os::unix::fs::symlink(elsewhere.path(), h.work.join("to-folder")).unwrap();
    std::os::unix::fs::symlink(elsewhere.path().join("nope"), h.work.join("dangling")).unwrap();
    for name in ["to-folder", "dangling"] {
        let e = refused(make(&h.engine, &parent, name).await);
        assert_eq!(e.code, ErrorCode::Conflict, "{name}");
        assert!(e.message.starts_with("A link called"), "{}", e.message);
    }
    assert!(!elsewhere.path().join("nope").exists());
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn tessera_s_own_folders_are_off_limits() {
    let h = harness(vec![], 100_000).await;
    // The harness keeps config and data under `dir`.
    let inside = h.dir.path().display().to_string();
    let e = refused(make(&h.engine, &inside, "mine").await);
    assert_eq!(e.code, ErrorCode::BadRequest);
    assert!(e.message.contains("Tessera's own"), "{}", e.message);
    assert!(!h.dir.path().join("mine").exists());
    // A link into them doesn't get around it.
    #[cfg(unix)]
    {
        let link = h.work.join("sneaky");
        std::os::unix::fs::symlink(h.dir.path(), &link).unwrap();
        let e = refused(make(&h.engine, &link.display().to_string(), "mine").await);
        assert!(e.message.contains("Tessera's own"), "{}", e.message);
        assert!(!h.dir.path().join("mine").exists());
    }
}
