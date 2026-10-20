use std::path::Path;

use serde_json::json;

use super::*;

#[test]
fn tag_ignores_trailing_ws() {
    assert_eq!(file_tag("a  \nb\n"), file_tag("a\nb\n"));
    assert_ne!(file_tag("a\nb\n"), file_tag("a\nc\n"));
    assert_eq!(file_tag("x").len(), 4);
}

#[test]
fn line_edits_use_original_numbering() {
    let src = "one\ntwo\nthree\nfour\n";
    let items = json!([
        {"at": "2", "text": "TWO"},
        {"at": ">3", "text": "three-and-a-half"},
        {"at": "<1", "text": "zero"},
        {"at": ">$", "text": "five"},
        {"at": "4-4", "text": ""}
    ]);
    let out = apply_line_edits(src, items.as_array().unwrap()).unwrap();
    assert_eq!(out, "zero\none\nTWO\nthree\nthree-and-a-half\nfive\n");
    let bad = json!([{"at": "1-3", "text": "x"}, {"at": "2", "text": "y"}]);
    assert!(
        apply_line_edits(src, bad.as_array().unwrap())
            .unwrap_err()
            .contains("overlap")
    );
    assert!(apply_line_edits(src, json!([{"at": "9", "text": ""}]).as_array().unwrap()).is_err());
}

#[test]
fn sed_style_ranges_are_ranges() {
    let src = "one\ntwo\nthree\nfour\n";
    let out = apply_line_edits(
        src,
        json!([{"at": "2,3", "text": "middle"}]).as_array().unwrap(),
    )
    .unwrap();
    assert_eq!(out, "one\nmiddle\nfour\n");
    let out = apply_line_edits(
        src,
        json!([{"at": "3,$", "text": "end"}]).as_array().unwrap(),
    )
    .unwrap();
    assert_eq!(out, "one\ntwo\nend\n");
    assert!(
        apply_line_edits(src, json!([{"at": "3,2", "text": "x"}]).as_array().unwrap()).is_err()
    );
}

#[test]
fn several_matches_come_back_with_the_call_that_would_work() {
    let src = "We recieve mail.\nThey recieve it too.\n";
    let args = json!({"path": "README.md", "replace": [{"new": "receive", "old": "recieve"}], "tag": "AF1B"});
    let err = apply_replacements_ext(src, args["replace"].as_array().unwrap()).unwrap_err();
    assert_eq!(err.all_of, Some((0, 2)));
    let fix = with_all(&args, 0).unwrap();
    assert_eq!(
        fix,
        json!({"path": "README.md", "replace": [{"old": "recieve", "new": "receive", "all": true}]})
    );
    // Sent as suggested, it applies.
    let r = apply_replacements_ext(src, fix["replace"].as_array().unwrap()).unwrap();
    assert_eq!(r.text, "We receive mail.\nThey receive it too.\n");
    // Too long to repeat back: only the hint.
    let long = "x".repeat(700);
    let args = json!({"path": "a", "replace": [{"old": long, "new": "y"}]});
    assert!(with_all(&args, 0).is_none());
}

#[test]
fn render_has_header_and_numbers() {
    let r = render_lines("f.txt", "a\nb\nc\n", 2, 1);
    assert!(r.starts_with("[f.txt#"));
    assert!(r.contains("2:b\n"));
    assert!(r.contains("offset=3"));
    let v = render_view("f.txt", "a\nb\nc\n", 2, 5);
    assert_eq!(v.shown, Some((2, 3)));
    assert_eq!(render_view("f.txt", "a\n", 5, 5).shown, None);
}

#[test]
fn a_read_stops_within_its_byte_budget_and_says_where_to_go_on() {
    let text: String = (1..=100)
        .map(|i| format!("{i:03}{}\n", "x".repeat(96)))
        .collect();
    let v = render_view_within("f.txt", &text, 1, 1000, 1_000);
    // Whole lines only, never more than the budget, and where to continue.
    assert_eq!(v.shown, Some((1, 9)));
    assert!(v.text.contains("lines 1-9 of 100"), "{}", v.text);
    assert!(v.text.ends_with("Continue with offset=10]"), "{}", v.text);
    // The next page picks up there; one line always shows, even over budget.
    assert_eq!(
        render_view_within("f.txt", &text, 10, 1000, 1_000).shown,
        Some((10, 18))
    );
    assert_eq!(
        render_view_within("f.txt", &text, 1, 1000, 10).shown,
        Some((1, 1))
    );
}

// ---- stale tags, rebase and seen lines ---------------------------------------

const P: &str = "/nonexistent/tessera-edit-test/f.rs";

fn target(cache: &FileCache) -> Target<'_> {
    Target {
        session: "s",
        cache: Some(cache),
        path: Path::new(P),
        display: "f.rs",
    }
}

fn lines(v: serde_json::Value) -> Vec<serde_json::Value> {
    v.as_array().unwrap().clone()
}

/// The model read all of `text`.
fn read_all(cache: &FileCache, text: &str) -> String {
    let tag = file_tag(text);
    cache.saw(
        "s",
        Path::new(P),
        &tag,
        Some(text),
        [(1, text.lines().count())],
    );
    tag
}

#[test]
fn stale_edit_is_rebased_past_an_unrelated_change_above() {
    let cache = FileCache::default();
    let v1 = "fn a() {}\n\nfn b() {\n    old();\n}\n";
    let t1 = read_all(&cache, v1);
    // Someone (a formatter, another edit) added lines at the top.
    let v2 = "use x;\nuse y;\n\nfn a() {}\n\nfn b() {\n    old();\n}\n";
    let (out, note) = target(&cache)
        .line_edit(
            v2,
            &t1,
            &lines(json!([{"at": "4", "text": "    new();"}, {"at": ">5", "text": "// end"}])),
        )
        .unwrap();
    assert_eq!(
        out,
        "use x;\nuse y;\n\nfn a() {}\n\nfn b() {\n    new();\n}\n// end\n"
    );
    assert!(note.unwrap().contains(&format!("Rebased from #{t1}")));
    // Inserts anchored before a line follow it too.
    let (out, _) = target(&cache)
        .line_edit(v2, &t1, &lines(json!([{"at": "<3", "text": "// b"}])))
        .unwrap();
    assert!(out.contains("// b\nfn b() {"), "{out}");
}

#[test]
fn stale_edit_touching_changed_lines_is_refused_with_the_current_text() {
    let cache = FileCache::default();
    let v1 = "a\nb\nc\nd\ne\n";
    let t1 = read_all(&cache, v1);
    let v2 = "a\nB!\nc\nd\ne\n";
    let t2 = file_tag(v2);
    let err = target(&cache)
        .line_edit(v2, &t1, &lines(json!([{"at": "1-2", "text": "x"}])))
        .unwrap_err();
    assert!(
        err.contains(&format!("changed since #{t1} (now #{t2})")),
        "{err}"
    );
    assert!(err.contains("item 1 at \"1-2\""), "{err}");
    assert!(err.contains("2:B!"), "shows the current text: {err}");
    assert!(err.contains(&format!("Retry with tag {t2}")), "{err}");
    // An insert anchored to a changed line conflicts as well.
    assert!(
        target(&cache)
            .line_edit(v2, &t1, &lines(json!([{"at": ">2", "text": "x"}])))
            .is_err()
    );
    // But an item on an unchanged line of the same request is fine alone.
    assert!(
        target(&cache)
            .line_edit(v2, &t1, &lines(json!([{"at": "4", "text": "D"}])))
            .is_ok()
    );
}

#[test]
fn a_block_the_diff_cannot_place_is_found_nearby() {
    let old = ["x", "}", "y", "}", "z"];
    let cur = ["x", "}", "NEW", "y", "}", "z"];
    let map = LineMap::from_lines(&old, &cur);
    assert_eq!(place_block(&map, &old, &cur, 3, 4), Some((4, 5)));
    // A block that moved: the diff keeps one side, the window finds the other.
    let old = ["a", "b", "c", "d"];
    let cur = ["c", "d", "a", "b"];
    let map = LineMap::from_lines(&old, &cur);
    assert_eq!(place_block(&map, &old, &cur, 1, 2), Some((3, 4)));
    assert_eq!(place_block(&map, &old, &cur, 3, 4), Some((1, 2)));
    // Duplicates within the window aren't guessed between.
    let old = ["a", "b", "s", "t"];
    let cur = ["s", "t", "a", "b", "a", "b"];
    let map = LineMap::from_lines(&old, &cur);
    let placed = place_block(&map, &old, &cur, 1, 2);
    let followed = matches!(map.map(0), Some(Mapped::Same(_)));
    assert!(followed || placed.is_none(), "{placed:?}");
    // A changed line is never "found".
    let cur = ["a", "B", "s", "t"];
    let map = LineMap::from_lines(&old, &cur);
    assert_eq!(place_block(&map, &old, &cur, 2, 2), None);
}

#[test]
fn unknown_stale_version_shows_current_lines() {
    let cache = FileCache::default();
    let cur = "one\ntwo\nthree\n";
    let t = file_tag(cur);
    let err = target(&cache)
        .line_edit(cur, "ABCD", &lines(json!([{"at": "2", "text": "x"}])))
        .unwrap_err();
    assert!(err.contains(&format!("(tag ABCD, now {t})")), "{err}");
    assert!(err.contains("1:one\n2:two\n3:three\n"), "{err}");
    // What the error showed now counts as seen.
    assert!(
        target(&cache)
            .line_edit(cur, &t, &lines(json!([{"at": "2", "text": "x"}])))
            .is_ok()
    );
}

#[test]
fn edits_to_lines_never_shown_are_refused_and_the_refusal_shows_them() {
    let cache = FileCache::default();
    let text: String = (1..=100).map(|i| format!("line {i}\n")).collect();
    let tag = file_tag(&text);
    // Read the first 20 lines; grep showed line 60 with one line of context.
    cache.saw("s", Path::new(P), &tag, Some(&text), [(1, 20)]);
    cache.saw("s", Path::new(P), &tag, None, [(59, 61)]);
    let t = target(&cache);
    assert!(
        t.line_edit(&text, &tag, &lines(json!([{"at": "5-10", "text": "x"}])))
            .is_ok()
    );
    assert!(
        t.line_edit(&text, &tag, &lines(json!([{"at": "59-61", "text": "x"}])))
            .is_ok(),
        "grep context counts"
    );
    assert!(
        t.line_edit(&text, &tag, &lines(json!([{"at": ">$", "text": "end"}])))
            .is_ok(),
        "appending needs nothing"
    );
    let err = t
        .line_edit(
            &text,
            &tag,
            &lines(json!([{"at": "40-42", "text": "x"}, {"at": ">70", "text": "y"}])),
        )
        .unwrap_err();
    assert!(
        err.starts_with("Refused: lines 40-42, 70 of f.rs weren't shown"),
        "{err}"
    );
    assert!(
        err.contains("41:line 41") && err.contains("70:line 70"),
        "{err}"
    );
    // Shown now: the retry goes through.
    assert!(
        t.line_edit(
            &text,
            &tag,
            &lines(json!([{"at": "40-42", "text": "x"}, {"at": ">70", "text": "y"}]))
        )
        .is_ok()
    );
    // With a forgotten read, require an informed retry too.
    let empty = FileCache::default();
    assert!(
        target(&empty)
            .line_edit(&text, &tag, &lines(json!([{"at": "90", "text": "x"}])))
            .is_err()
    );
}

#[test]
fn big_refusals_are_capped_and_point_at_the_rest() {
    let cache = FileCache::default();
    let text: String = (1..=200).map(|i| format!("l{i}\n")).collect();
    let tag = file_tag(&text);
    cache.saw("s", Path::new(P), &tag, Some(&text), [(1, 1)]);
    let err = target(&cache)
        .line_edit(&text, &tag, &lines(json!([{"at": "100-180", "text": "x"}])))
        .unwrap_err();
    assert_eq!(
        err.lines()
            .filter(|l| l.starts_with('l') || l.contains(":l"))
            .count(),
        EXCERPT_LINES
    );
    assert!(err.contains("read offset=139"), "{err}");
}

// ---- replace chain ---------------------------------------------------------

fn rep(src: &str, items: serde_json::Value) -> Result<(String, Vec<String>), String> {
    apply_replacements(src, items.as_array().unwrap())
}

#[test]
fn replacements_exact_trailing_space_and_ambiguous() {
    let src = "fn a() {\n    x();   \n}\nfn b() {\n    x();\n}\n";
    let (r, notes) = rep(
        src,
        json!([{"old": "fn b() {\n    x();", "new": "fn b() {\n    y();"}]),
    )
    .unwrap();
    assert!(r.contains("y();") && r.matches("x();").count() == 1);
    assert!(notes.is_empty(), "exact match needs no note");
    let amb = rep(src, json!([{"old": "x();", "new": "z();"}])).unwrap_err();
    assert!(amb.contains("matches 2 places"));
    // Trailing whitespace in the file does not block a match.
    let (fz, _) = rep("a  \nb\n", json!([{"old": "a\nb", "new": "c"}])).unwrap();
    assert_eq!(fz, "c\n");
    let (crlf, _) = rep("a\r\nb\r\n", json!([{"old": "a\nb", "new": "x\ny"}])).unwrap();
    assert_eq!(crlf, "x\r\ny\r\n");
}

#[test]
fn indentation_differences_are_matched_and_new_text_reindented() {
    let src =
        "impl A {\n    fn f(&self) {\n        if x {\n            go();\n        }\n    }\n}\n";
    // The model dropped the indentation entirely.
    let (out, notes) = rep(
        src,
        json!([{"old": "if x {\n    go();\n}", "new": "if x {\n    go();\n    again();\n}"}]),
    )
    .unwrap();
    assert_eq!(
        out,
        "impl A {\n    fn f(&self) {\n        if x {\n            go();\n            again();\n        }\n    }\n}\n"
    );
    assert!(notes[0].contains("ignoring indentation"), "{notes:?}");
    // Relative indentation must still match.
    assert!(
        rep(src, json!([{"old": "if x {\ngo();\n}", "new": "y"}])).is_ok(),
        "falls to the trimmed strategy"
    );
    let (_, notes) = rep(src, json!([{"old": "if x {\ngo();\n}", "new": "y"}])).unwrap();
    assert!(notes[0].contains("trimmed"), "{notes:?}");
    // CRLF survives the loose strategies.
    let crlf = src.replace('\n', "\r\n");
    let (out, _) = rep(
        &crlf,
        json!([{"old": "if x {\n    go();\n}", "new": "if x {\n    stop();\n}"}]),
    )
    .unwrap();
    assert!(
        out.contains("        if x {\r\n            stop();\r\n        }\r\n"),
        "{out:?}"
    );
    assert!(!out.replace("\r\n", "").contains('\n'));
}

#[test]
fn trimmed_matching_collapses_spaces_but_needs_whole_lines() {
    let src = "let a =  1;\n\tlet b = 2;\n";
    let (out, _) = rep(
        src,
        json!([{"old": "let a = 1;\n  let b  =  2;", "new": "let a = 3;\n\tlet b = 4;"}]),
    )
    .unwrap();
    assert_eq!(out, "let a = 3;\n\tlet b = 4;\n");
    // A fragment of a line is never matched loosely.
    let err = rep(src, json!([{"old": "a  =   1", "new": "q"}])).unwrap_err();
    assert!(err.contains("not found"), "{err}");
    // Spacing inside lines is only ignored for enough text to be meaningful.
    assert!(
        rep("a   b\n", json!([{"old": "a b", "new": "y"}]))
            .unwrap_err()
            .contains("not found")
    );
    assert_eq!(
        rep("ab   c\n", json!([{"old": "ab c", "new": "y"}]))
            .unwrap()
            .0,
        "y\n"
    );
    // Indentation alone may differ even for a tiny line.
    let (out, notes) = rep("  x\n", json!([{"old": " x ", "new": "y"}])).unwrap();
    assert_eq!(out, "y\n");
    assert!(notes[0].contains("indentation"), "{notes:?}");
}

#[test]
fn loose_matches_must_be_unique_unless_all() {
    let src = "  a();\n  b();\n\n    a();\n    b();\n";
    let err = rep(src, json!([{"old": "a();\nb();", "new": "c();"}])).unwrap_err();
    assert!(err.contains("fits 2 places (lines [1, 4])"), "{err}");
    assert!(err.contains("set `all`"), "{err}");
    let (out, notes) = rep(
        src,
        json!([{"old": "a();\nb();", "new": "c();", "all": true}]),
    )
    .unwrap();
    assert_eq!(out, "  c();\n\n    c();\n");
    assert!(notes[0].contains("(2 places)"), "{notes:?}");
}

#[test]
fn span_guard_refuses_oversized_loose_matches() {
    // Needle lines are tiny; the file's lines are padded with huge runs of
    // spaces, so the loose match covers far more text than `old`.
    let pad = " ".repeat(400);
    let src = format!("a{pad}b\nc{pad}d\n");
    let err = loose_replace(&src, "a b\nc d", "x", false).unwrap_err();
    assert!(matches!(err, LooseError::TooLarge { .. }), "{err:?}");
    let msg = rep(&src, json!([{"old": "a b\nc d", "new": "x"}])).unwrap_err();
    assert!(msg.contains("refusing"), "{msg}");
}

#[test]
fn replacements_are_placed_in_the_file_as_it_was_whatever_their_order() {
    let src = "alpha();\nbeta();\ngamma();\n";
    // Given out of file order.
    let (out, _) = rep(
        src,
        json!([{"old": "gamma();", "new": "c();"}, {"old": "alpha();", "new": "a();"}]),
    )
    .unwrap();
    assert_eq!(out, "a();\nbeta();\nc();\n");
    // One item's new text holds another's old text: placed against the
    // original, the second still matches only its own place.
    let (out, _) = rep(
        src,
        json!([{"old": "alpha();", "new": "alpha();\nbeta();"}, {"old": "beta();", "new": "b();"}]),
    )
    .unwrap();
    assert_eq!(out, "alpha();\nbeta();\nb();\ngamma();\n");
    // A chain (the second edits what the first wrote) still works in order.
    let (out, _) = rep(
        src,
        json!([{"old": "alpha();", "new": "delta();"}, {"old": "delta();", "new": "epsilon();"}]),
    )
    .unwrap();
    assert_eq!(out, "epsilon();\nbeta();\ngamma();\n");
    // The same item twice is applied once.
    let (out, notes) = rep(
        src,
        json!([{"old": "beta();", "new": "b();"}, {"old": "beta();", "new": "b();"}]),
    )
    .unwrap();
    assert_eq!(out, "alpha();\nb();\ngamma();\n");
    assert!(notes[0].contains("repeats an earlier one"), "{notes:?}");
    // Overlapping items fall back to order: the second sees the first's result.
    let (out, _) = rep(
        src,
        json!([{"old": "alpha();\nbeta();", "new": "ab();"}, {"old": "ab();", "new": "x();"}]),
    )
    .unwrap();
    assert_eq!(out, "x();\ngamma();\n");
}

#[test]
fn a_change_already_in_the_file_is_reported_not_failed() {
    let src = "fn main() {\n    println!(\"hello, world\");\n}\n";
    let items = json!([{"old": "    println!(\"hi\");", "new": "    println!(\"hello, world\");"}]);
    let r = apply_replacements_ext(src, items.as_array().unwrap()).unwrap();
    assert_eq!(r.text, src);
    assert_eq!(r.already, vec!["replacement 1 (line 2)"]);
    // Together with a real change: applied, with a note.
    let items = json!([
        {"old": "    println!(\"hi\");", "new": "    println!(\"hello, world\");"},
        {"old": "fn main() {", "new": "pub fn main() {"}
    ]);
    let r = apply_replacements_ext(src, items.as_array().unwrap()).unwrap();
    assert!(r.text.starts_with("pub fn main() {"));
    assert!(
        r.notes.iter().any(|n| n.contains("already is, at line 2")),
        "{:?}",
        r.notes
    );
    // A short `new` found elsewhere proves nothing: still "not found".
    let err = rep(src, json!([{"old": "foo()", "new": "main"}])).unwrap_err();
    assert!(err.contains("not found"), "{err}");
}

#[test]
fn no_match_shows_the_closest_block_with_lines_and_similarity() {
    let mut src = String::new();
    for i in 0..200 {
        src.push_str(&format!("fn f{i}() -> u32 {{ {i} }}\n"));
    }
    src.push_str("fn total(items: &[Item]) -> u32 {\n    let mut s = 0;\n    for it in items {\n        s += it.price;\n    }\n    s\n}\n");
    let err = rep(
        &src,
        json!([{"old": "    let mut sum = 0;\n    for it in items {\n        sum += it.price;\n    }", "new": "x"}]),
    )
    .unwrap_err();
    assert!(
        err.contains("The closest block is lines 202-205 ("),
        "{err}"
    );
    assert!(err.contains("% similar):\n202:    let mut s = 0;\n203:    for it in items {\n204:        s += it.price;\n205:    }\n"), "{err}");
    // Nothing alike: said plainly.
    let err = rep(
        "a\nb\n",
        json!([{"old": "completely different text here", "new": "x"}]),
    )
    .unwrap_err();
    assert!(err.contains("Nothing in the file is close to it"), "{err}");
    // Long blocks are capped.
    let big: String = (0..30)
        .map(|i| format!("line number {i} of the block\n"))
        .collect();
    let needle = big.replace("number", "nunber");
    let err = rep(&big, json!([{"old": needle, "new": "x"}])).unwrap_err();
    assert!(
        err.contains("lines 1-30") && err.contains("… (18 more lines)"),
        "{err}"
    );
}

// ---- parse check -----------------------------------------------------------

#[test]
fn data_files_are_parse_checked_after_edits() {
    let j = Path::new("/x/package.json");
    assert_eq!(
        parse_check(j, "package.json", Some("{}"), "{\"a\": 1}"),
        None
    );
    let w = parse_check(j, "package.json", Some("{}"), "{\"a\": 1,}").unwrap();
    assert!(
        w.starts_with("Warning: package.json no longer parses as JSON:"),
        "{w}"
    );
    assert!(w.contains("line 1"), "{w}");
    let w = parse_check(j, "package.json", Some("{"), "{,").unwrap();
    assert!(w.contains("still doesn't parse"), "{w}");
    let w = parse_check(j, "package.json", None, "nope").unwrap();
    assert!(
        w.starts_with("Warning: package.json doesn't parse as JSON"),
        "{w}"
    );
    // JSON-with-comments files are judged leniently.
    let ts = Path::new("/x/tsconfig.json");
    assert_eq!(
        parse_check(
            ts,
            "tsconfig.json",
            Some("{}"),
            "{\n  // c\n  \"a\": [1, 2,],\n}\n"
        ),
        None
    );
    let vs = Path::new("/x/other.json");
    let commented = "{ /* keep */ \"a\": 1 }";
    assert_eq!(
        parse_check(vs, "other.json", Some(commented), "{ /* keep */ \"a\": 2 }"),
        None
    );
    assert!(parse_check(vs, "other.json", Some(commented), "{ \"a\": }").is_some());

    let t = Path::new("/x/Cargo.toml");
    assert_eq!(
        parse_check(t, "Cargo.toml", Some(""), "[package]\nname = \"x\"\n"),
        None
    );
    let w = parse_check(t, "Cargo.toml", Some(""), "[package]\nname = \n").unwrap();
    assert!(
        w.contains("no longer parses as TOML") && w.contains("line 2"),
        "{w}"
    );

    let y = Path::new("/x/ci.yml");
    assert_eq!(
        parse_check(y, "ci.yml", Some("a: 1\n"), "a: 1\nb:\n  - c\n---\nd: 2\n"),
        None
    );
    let w = parse_check(y, "ci.yml", Some("a: 1\n"), "a: [1, 2\nb: 3\n").unwrap();
    assert!(w.contains("no longer parses as YAML"), "{w}");
    assert_eq!(parse_check(Path::new("/x/a.rs"), "a.rs", None, "{"), None);
}

#[test]
fn jsonc_stripping_respects_strings() {
    let s = strip_jsonc("{\"url\": \"http://x//y\", /* c */ \"b\": [1,],}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&s).unwrap()["url"],
        "http://x//y"
    );
}

#[test]
fn writes_that_make_something_run_later_are_sensitive() {
    for p in [
        "/home/u/.bashrc",
        "/home/u/proj/.git/hooks/pre-commit",
        "/home/u/proj/.git/config",
        "/home/u/.gitconfig",
        "/home/u/.config/git/config",
        "/home/u/.local/bin/ls",
        "/home/u/.config/fish/config.fish",
        "/home/u/.config/hypr/hyprland.conf",
        "/home/u/proj/.envrc",
        "/home/u/proj/.claude/settings.json",
        "/home/u/proj/.claude/settings.local.json",
        "/home/u/.claude/settings.json",
        "/home/u/proj/.mcp.json",
    ] {
        assert!(sensitive(Path::new(p)), "{p}");
    }
    for p in [
        "/home/u/proj/src/main.rs",
        "/home/u/proj/.gitignore",
        "/home/u/proj/config",
        "/home/u/proj/docs/claude.md",
    ] {
        assert!(!sensitive(Path::new(p)), "{p}");
    }
}

#[test]
fn a_read_names_one_file_or_several() {
    assert_eq!(read_paths(&json!({"path": "a.py"})), ["a.py"]);
    assert_eq!(
        read_paths(&json!({"paths": ["a.py", "b.md", "a.py", " "]})),
        ["a.py", "b.md"]
    );
    // Both given: `path` first, then the others, without repeats.
    assert_eq!(
        read_paths(&json!({"path": "a.py", "paths": ["b.md", "a.py"]})),
        ["a.py", "b.md"]
    );
    // A list under `path`, one name under `paths`.
    assert_eq!(read_paths(&json!({"path": ["a", "b"]})), ["a", "b"]);
    assert_eq!(read_paths(&json!({"paths": "a"})), ["a"]);
    assert!(read_paths(&json!({})).is_empty());
}

#[test]
fn several_files_share_one_reads_size_small_ones_whole() {
    // Two small files come whole; the big ones split what is left.
    let s = shares(
        &[Some(100_000), Some(300), Some(80_000), Some(1_000)],
        28_000,
    );
    assert!(s[1] >= 450 && s[3] >= 1_500, "{s:?}");
    assert!(s.iter().sum::<usize>() <= 28_000, "{s:?}");
    assert!(s[0] > 12_000 && s[2] > 12_000, "{s:?}");
    // Never more than the budget, however many files (no floor).
    let s = shares(&[Some(1 << 20); 20], 28_000);
    assert!(s.iter().sum::<usize>() <= 28_000, "{s:?}");
    // Nothing for what has no lines.
    assert_eq!(shares(&[None, Some(10)], 1_000), vec![0, 271]);
}

#[test]
fn a_read_of_several_files_plans_within_its_size_headers_included() {
    // 20 big files at a 4 KB tool output limit (7,168 bytes for one read):
    // only as many as fit with their headers, the others left out with a
    // line each; each one read gets at least a full line.
    let fixed = vec![10 + PART_BYTES; 20];
    let left = vec![10 + LEFT_OUT_BYTES; 20];
    let sizes = vec![Some(1u64 << 20); 20];
    let (count, s) = plan_reads(&fixed, &left, &sizes, 7_168).unwrap();
    assert!((1..20).contains(&count), "{count}");
    let total = fixed[..count].iter().sum::<usize>()
        + left[count..].iter().sum::<usize>()
        + s.iter().sum::<usize>();
    assert!(total <= 7_168, "{total}");
    assert!(s.iter().all(|&b| b >= MIN_LINES_BYTES), "{s:?}");
    // With room for all of them, all come.
    let (count, s) = plan_reads(&fixed, &left, &sizes, 96 * 1024).unwrap();
    assert_eq!(count, 20);
    assert!(s.iter().sum::<usize>() + fixed.iter().sum::<usize>() <= 96 * 1024);
    // A message without lines (an error) needs no room for lines.
    let (count, _) = plan_reads(&[60, 60], &[110, 110], &[None, None], 2_048).unwrap();
    assert_eq!(count, 2);
    // Not even one fits: nothing is read.
    assert_eq!(plan_reads(&fixed, &left, &sizes, 2_048), None);
}

#[test]
fn an_edit_with_nothing_to_change_ends_with_the_call_that_would_work() {
    let bare = nothing_to_change(&json!({"path": "src/a.py"}));
    assert!(
        bare.starts_with("This `edit` has nothing to change. Send {\"path\": \"src/a.py\", \"replace\": [{\"old\""),
        "{bare}"
    );
    assert!(
        bare.contains("`lines` items") && bare.contains("`tag`"),
        "{bare}"
    );
    // Text without a place: a whole new version goes through `write`.
    for key in ["text", "content", "new"] {
        let m = nothing_to_change(&json!({"path": "a.py", key: "x = 1\n"}));
        assert!(
            m.contains("call `write` with {\"path\": \"a.py\", \"content\": …}"),
            "{key}: {m}"
        );
    }
    // A path with quotes stays valid JSON in the suggestion.
    let odd = nothing_to_change(&json!({"path": "say \"hi\".txt"}));
    assert!(
        odd.contains(r#"{"path": "say \"hi\".txt", "replace""#),
        "{odd}"
    );
}

#[test]
fn a_guessed_current_tag_does_not_make_unread_lines_editable() {
    let cache = FileCache::default();
    let cur = "one\ntwo\nthree\n";
    let tag = file_tag(cur);
    let err = target(&cache)
        .line_edit(cur, &tag, &lines(json!([{"at":"2", "text":"TWO"}])))
        .unwrap_err();
    assert!(err.contains("weren't shown"), "{err}");
    assert!(err.contains("2:two"), "{err}");
    // The refusal really showed the line: the informed retry can apply.
    assert!(
        target(&cache)
            .line_edit(cur, &tag, &lines(json!([{"at":"2", "text":"TWO"}])))
            .is_ok()
    );
}

#[test]
fn the_write_run_reader_rejects_growth_after_the_earlier_size_check() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("growing.txt");
    std::fs::write(&path, "small old content\n").unwrap();
    let opened = super::super::file_object::Opened::open(&path).unwrap();
    assert!(opened.file.metadata().unwrap().len() < MAX_WRITE_BYTES);
    // Grow the same inode after the advisory check/open, not a different path.
    let grown = MAX_WRITE_BYTES + 1;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(grown)
        .unwrap();
    assert!(
        bounded_write_bytes(&opened)
            .unwrap_err()
            .contains("size limit")
    );
    assert_eq!(std::fs::metadata(path).unwrap().len(), grown);
}
