//! Scripted chats for judging when a conversation is worth a skill: used by
//! `live_skills.rs` (through the engine) and `live_choices.rs` (the review
//! prompt on its own, before and after tuning).

use serde_json::json;

use super::Reply;

/// One step of the agent's turn.
#[derive(Clone, Copy)]
pub enum Step {
    /// A command whose output is canned: the text shown is what a real run
    /// would type, the output what it would have printed.
    Sh(&'static str, &'static str),
    /// A command that really runs in the chat's folder (with the output a
    /// transcript shows for it).
    Real(&'static str, &'static str),
    /// The agent's reply.
    Say(&'static str),
}

impl Step {
    pub fn reply(&self) -> Reply {
        match self {
            Step::Sh(shown, output) => Reply::Tools(vec![(
                "bash".into(),
                json!({ "command": format!("# {shown}\ncat <<'EOF'\n{output}\nEOF") }),
            )]),
            Step::Real(cmd, _) => Reply::Tools(vec![("bash".into(), json!({ "command": cmd }))]),
            Step::Say(t) => Reply::Text((*t).into()),
        }
    }
}

pub struct Case {
    pub name: &'static str,
    /// A procedure worth a skill (otherwise nothing should be created).
    pub reusable: bool,
    /// Files the chat starts with.
    pub files: &'static [(&'static str, &'static str)],
    /// User messages, each with the agent's steps that follow.
    pub turns: Vec<(&'static str, Vec<Step>)>,
}

impl Case {
    /// Tool calls in the chat.
    pub fn work(&self) -> usize {
        self.turns
            .iter()
            .flat_map(|(_, steps)| steps)
            .filter(|s| !matches!(s, Step::Say(_)))
            .count()
    }

    /// Whether the user's words ask for a skill.
    pub fn asked(&self) -> bool {
        self.turns
            .iter()
            .any(|(user, _)| tessera_core::skills::asks_for_skill(user))
    }

    /// The chat as the review reads it.
    pub fn transcript(&self) -> String {
        let mut out = String::new();
        let mut n = 0;
        let mut next = || {
            n += 1;
            n
        };
        for (user, steps) in &self.turns {
            out.push_str(&format!("[User #{}]: {user}\n\n", next()));
            for s in steps {
                match s {
                    Step::Sh(shown, output) | Step::Real(shown, output) => {
                        out.push_str(&format!("[Tool call] bash command={shown}\n"));
                        out.push_str(&format!("[Tool result bash #{}]: {output}\n\n", next()));
                    }
                    Step::Say(t) => out.push_str(&format!("[Assistant]: {t}\n\n")),
                }
            }
        }
        out
    }
}

pub fn cases() -> Vec<Case> {
    use Step::*;
    vec![
        Case {
            name: "rename-file",
            reusable: false,
            files: &[("notes.txt", "agenda\n")],
            turns: vec![(
                "Rename notes.txt to meeting-notes.txt.",
                vec![
                    Real("mv notes.txt meeting-notes.txt && ls", "meeting-notes.txt"),
                    Say("Renamed notes.txt to meeting-notes.txt."),
                ],
            )],
        },
        Case {
            name: "quick-answer",
            reusable: false,
            files: &[],
            turns: vec![(
                "What's the capital of Australia, and how far behind is it from London right now?",
                vec![Say(
                    "The capital is Canberra. It is 10 hours ahead of London in winter and 9 hours ahead in the northern summer.",
                )],
            )],
        },
        Case {
            name: "hello-script",
            reusable: false,
            files: &[],
            turns: vec![(
                "Write a Python script squares.py that prints the first 10 squares, and run it.",
                vec![
                    Real(
                        "printf 'for i in range(1, 11):\\n    print(i * i)\\n' > squares.py",
                        "",
                    ),
                    Real("python3 squares.py", "1\n4\n9\n16\n25\n36\n49\n64\n81\n100"),
                    Say("squares.py prints 1, 4, 9 … 100. I ran it and it works."),
                ],
            )],
        },
        Case {
            name: "one-off-bugfix",
            reusable: false,
            files: &[],
            turns: vec![(
                "tests/test_totals.py fails in this project, please fix it.",
                vec![
                    Sh(
                        "pytest -q tests/test_totals.py",
                        "F\nE   assert 108.0 == 100.0\ntests/test_totals.py:6: AssertionError\n1 failed in 0.03s",
                    ),
                    Sh(
                        "cat tests/test_totals.py",
                        "from totals import total\n\ndef test_total_adds_tax():\n    assert total(100.0) == 108.0\n",
                    ),
                    Sh(
                        "cat totals.py",
                        "TAX = 0.8\n\ndef total(x):\n    return x * (1 + TAX)\n",
                    ),
                    Sh("sed -i 's/TAX = 0.8/TAX = 0.08/' totals.py", ""),
                    Sh("pytest -q tests/test_totals.py", ".\n1 passed in 0.02s"),
                    Say(
                        "The tax constant in totals.py was 0.8 instead of 0.08. Fixed; the test passes.",
                    ),
                ],
            )],
        },
        Case {
            name: "personal-facts",
            reusable: false,
            files: &[],
            turns: vec![(
                "My NAS is at 192.168.1.50 (user admin) and I prefer metric units in answers.",
                vec![Say(
                    "Got it: NAS at 192.168.1.50 with user admin, metric units from now on.",
                )],
            )],
        },
        Case {
            name: "repo-summary",
            reusable: false,
            files: &[
                ("README.md", "# Widget\nA small widget library.\n"),
                ("src/lib.rs", "pub fn widget() {}\n"),
            ],
            turns: vec![(
                "Give me a quick summary of what is in this folder.",
                vec![
                    Real("ls -la", "README.md\nsrc"),
                    Real("cat README.md", "# Widget\nA small widget library."),
                    Say(
                        "It is a small Rust widget library: README.md describes it and src/lib.rs holds a single `widget` function.",
                    ),
                ],
            )],
        },
        // Several steps, even a failure and its fix, but nothing a capable
        // agent would not work out again, and nothing that will recur.
        Case {
            name: "venv-setup",
            reusable: false,
            files: &[],
            turns: vec![(
                "Set up a virtualenv for this project, install requirements.txt and run the tests.",
                vec![
                    Sh("python3 -m venv .venv", ""),
                    Sh(
                        ".venv/bin/pip install -r requirements.txt",
                        "Successfully installed flask-3.0.3 pytest-8.2.0",
                    ),
                    Sh(".venv/bin/pytest -q", "......\n6 passed in 0.41s"),
                    Sh("git status --short", "?? .venv/"),
                    Say("Done: .venv created, requirements installed, 6 tests pass."),
                ],
            )],
        },
        Case {
            name: "heic-convert",
            reusable: false,
            files: &[],
            turns: vec![(
                "Convert all the .heic photos in ~/Pictures/trip to jpg.",
                vec![
                    Sh("which heif-convert convert", "/usr/bin/convert"),
                    Sh(
                        "convert ~/Pictures/trip/IMG_001.heic /tmp/x.jpg",
                        "convert: no decode delegate for this image format `HEIC'",
                    ),
                    Sh(
                        "sudo apt install -y libheif-examples",
                        "Setting up libheif-examples",
                    ),
                    Sh(
                        "cd ~/Pictures/trip && for f in *.heic; do heif-convert \"$f\" \"${f%.heic}.jpg\"; done",
                        "Written to IMG_001.jpg\nWritten to IMG_002.jpg\nWritten to IMG_003.jpg",
                    ),
                    Say(
                        "Converted the 3 HEIC photos to JPG with heif-convert (ImageMagick lacked the HEIC decoder, so I installed libheif-examples).",
                    ),
                ],
            )],
        },
        Case {
            name: "port-conflict",
            reusable: false,
            files: &[],
            turns: vec![(
                "My docker compose stack won't start, can you see why?",
                vec![
                    Sh(
                        "docker compose up -d",
                        "Error: Bind for 0.0.0.0:8080 failed: port is already allocated",
                    ),
                    Sh(
                        "ss -ltnp | grep 8080",
                        "LISTEN 0 4096 0.0.0.0:8080 users:((\"grafana\",pid=1411,fd=7))",
                    ),
                    Sh("sed -i 's/8080:80/8081:80/' compose.yaml", ""),
                    Sh("docker compose up -d", "Container web  Started"),
                    Sh("curl -sI localhost:8081 | head -1", "HTTP/1.1 200 OK"),
                    Say(
                        "Grafana was already using port 8080, so I moved the web service to 8081 in compose.yaml. It is up and answers 200.",
                    ),
                ],
            )],
        },
        Case {
            name: "git-undo",
            reusable: false,
            files: &[],
            turns: vec![(
                "I committed to main by mistake, undo that but keep my changes and put them on a new branch called fix-typo.",
                vec![
                    Sh(
                        "git log --oneline -3",
                        "a1b2c3d Fix typo in README\n9f8e7d6 Release 0.3\n",
                    ),
                    Sh("git branch fix-typo", ""),
                    Sh(
                        "git reset --hard HEAD~1",
                        "HEAD is now at 9f8e7d6 Release 0.3",
                    ),
                    Sh(
                        "git switch fix-typo && git log --oneline -1",
                        "a1b2c3d Fix typo in README",
                    ),
                    Say("Done: the commit now lives on fix-typo and main is back at Release 0.3."),
                ],
            )],
        },
        Case {
            name: "canvas-api",
            reusable: true,
            files: &[],
            turns: vec![
                (
                    "Use my Canvas token to list my assignments due this week.",
                    vec![
                        Sh(
                            "curl -s -H \"Authorization: Bearer $CANVAS_TOKEN\" https://canvas.example.edu/api/v1/assignments",
                            "HTTP 404 {\"errors\":[{\"message\":\"The specified resource does not exist.\"}]}",
                        ),
                        Sh(
                            "curl -s -H \"Authorization: Bearer $CANVAS_TOKEN\" https://canvas.example.edu/api/v1/users/self/assignments",
                            "HTTP 404 {\"errors\":[{\"message\":\"The specified resource does not exist.\"}]}",
                        ),
                        Say(
                            "Both endpoints return 404, so API access seems to be disabled for your Canvas account.",
                        ),
                    ],
                ),
                (
                    "API access is not disabled, my other scripts use it. Assignments are per course: list my courses first. I'm in Chicago.",
                    vec![
                        Sh(
                            "curl -si -H \"Authorization: Bearer $CANVAS_TOKEN\" 'https://canvas.example.edu/api/v1/courses?enrollment_state=active&per_page=100'",
                            "HTTP 200\nLink: <https://canvas.example.edu/api/v1/courses?enrollment_state=active&page=2&per_page=100>; rel=\"next\"\n[{\"id\":101,\"name\":\"Biology\"},{\"id\":102,\"name\":\"History\"}]",
                        ),
                        Sh(
                            "curl -s -H \"Authorization: Bearer $CANVAS_TOKEN\" 'https://canvas.example.edu/api/v1/courses?enrollment_state=active&page=2&per_page=100'",
                            "HTTP 200\n[{\"id\":103,\"name\":\"Statistics\"}]",
                        ),
                        Sh(
                            "for c in 101 102 103; do curl -s ... /courses/$c/assignments?bucket=upcoming&per_page=100; done",
                            "course 101: [{\"name\":\"Lab report\",\"due_at\":\"2026-10-02T04:59:00Z\"}]\ncourse 102: []\ncourse 103: [{\"name\":\"Problem set 4\",\"due_at\":\"2026-10-03T04:59:00Z\"}]",
                        ),
                        Say(
                            "Due this week: Lab report (Biology), Oct 1 at 11:59 PM, and Problem set 4 (Statistics), Oct 2 at 11:59 PM, Chicago time. How it works: list active courses with GET /api/v1/courses?enrollment_state=active&per_page=100 and follow the Link header's rel=\"next\" (there were 2 pages), then GET /api/v1/courses/<id>/assignments?bucket=upcoming for each course. due_at is UTC, so convert to America/Chicago.",
                        ),
                    ],
                ),
            ],
        },
        Case {
            name: "deploy-blog",
            reusable: true,
            files: &[],
            turns: vec![
                (
                    "Deploy the blog to my homelab.",
                    vec![
                        Sh(
                            "hugo --minify",
                            "Start building sites …\nTotal in 412 ms\nWrote public/ (baseURL http://localhost:1313/)",
                        ),
                        Sh(
                            "rsync -a --delete public homelab:/srv/blog/",
                            "sent 1.2M bytes\n(created /srv/blog/public/ — the site now lives one folder too deep)",
                        ),
                        Say("Deployed to /srv/blog/public. Let me know if it looks right."),
                    ],
                ),
                (
                    "No. The site is served from /srv/blog itself and the links point at localhost. Build with the real base URL https://blog.ken.lan/ and never delete .well-known, my certificates are in there. Reload caddy afterwards and check it answers 200.",
                    vec![
                        Sh(
                            "hugo --minify --baseURL https://blog.ken.lan/",
                            "Total in 398 ms\nWrote public/ (baseURL https://blog.ken.lan/)",
                        ),
                        Sh(
                            "rsync -a --delete --exclude .well-known public/ homelab:/srv/blog/",
                            "sent 1.2M bytes, 14 files updated",
                        ),
                        Sh("ssh homelab 'sudo systemctl reload caddy'", ""),
                        Sh("curl -sI https://blog.ken.lan | head -1", "HTTP/2 200"),
                        Say(
                            "Done: the site is live at https://blog.ken.lan/ (200). Procedure from now on: hugo --minify --baseURL https://blog.ken.lan/, then rsync -a --delete --exclude .well-known public/ homelab:/srv/blog/ (trailing slash on public/), then ssh homelab 'sudo systemctl reload caddy', then curl -sI https://blog.ken.lan expecting 200.",
                        ),
                    ],
                ),
            ],
        },
        Case {
            name: "cut-release",
            reusable: true,
            files: &[],
            turns: vec![
                (
                    "Cut release 1.4.0 of the CLI.",
                    vec![
                        Sh(
                            "sed -i 's/^version = .*/version = \"1.4.0\"/' Cargo.toml",
                            "",
                        ),
                        Sh("cargo test", "test result: ok. 212 passed; 0 failed"),
                        Sh(
                            "git tag v1.4.0 && git push origin main --tags",
                            "To github.com:ken/cli.git\n * [new tag] v1.4.0 -> v1.4.0",
                        ),
                        Say("Tagged v1.4.0 and pushed."),
                    ],
                ),
                (
                    "You missed things. Every release also needs packaging/arch/PKGBUILD pkgver and docs/conf.py bumped, the CHANGELOG.md entry (## [1.4.0] - date, grouped Added/Fixed), and tags are always signed and annotated: git tag -s v1.4.0 -m 'v1.4.0'. Then publish with gh release create v1.4.0 --notes-file from the changelog entry.",
                    vec![
                        Sh(
                            "sed -i 's/^pkgver=.*/pkgver=1.4.0/' packaging/arch/PKGBUILD && sed -i \"s/^release = .*/release = '1.4.0'/\" docs/conf.py",
                            "",
                        ),
                        Sh(
                            "edit CHANGELOG.md: add '## [1.4.0] - 2026-10-01' with Added/Fixed groups",
                            "CHANGELOG.md updated",
                        ),
                        Sh(
                            "git tag -d v1.4.0 && git push origin :refs/tags/v1.4.0 && git tag -s v1.4.0 -m 'v1.4.0' && git push origin main --follow-tags",
                            "deleted remote tag\n * [new tag] v1.4.0 -> v1.4.0 (signed)",
                        ),
                        Sh(
                            "gh release create v1.4.0 --notes-file /tmp/notes-1.4.0.md",
                            "https://github.com/ken/cli/releases/tag/v1.4.0",
                        ),
                        Say(
                            "Release 1.4.0 is out. Checklist for next time: bump Cargo.toml, packaging/arch/PKGBUILD pkgver and docs/conf.py; CHANGELOG.md entry '## [1.4.0] - date' grouped Added/Fixed; cargo test; signed annotated tag git tag -s vX.Y.Z -m 'vX.Y.Z'; git push origin main --follow-tags; gh release create vX.Y.Z --notes-file <changelog entry>.",
                        ),
                    ],
                ),
            ],
        },
        Case {
            name: "asked-for-skill",
            reusable: true,
            files: &[],
            turns: vec![(
                "Every Monday I need standup notes: take my merged pull requests of the last 7 days (gh pr list --author @me --state merged --search \"merged:>=DATE\"), group them by repo, one line each, skip anything from dependabot, newest first. Do it for this week and save this as a skill so you do it the same way next time.",
                vec![
                    Sh(
                        "gh pr list --author @me --state merged --search \"merged:>=2026-09-24\" --json title,repository,author,mergedAt",
                        "[{\"title\":\"Fix reconnect race\",\"repository\":\"tessera\",\"author\":\"ken\"},{\"title\":\"Bump serde\",\"repository\":\"tessera\",\"author\":\"dependabot\"},{\"title\":\"Add webview setting\",\"repository\":\"tessera\",\"author\":\"ken\"}]",
                    ),
                    Say(
                        "Standup notes, week of Sep 24:\n\ntessera\n- Add webview setting\n- Fix reconnect race\n\n(dependabot's serde bump skipped.)",
                    ),
                ],
            )],
        },
    ]
}
