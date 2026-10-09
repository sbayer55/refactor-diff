//! The context pieces and the task menu on the fixture projects (the port of
//! `tests/test_ai_context.py`).

use std::collections::BTreeSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;
use refactor_diff_core::{FileChange, FileStatus, NoHeadFiles, Report, Source, StmtKind};
use refactor_diff_server::ai::context::{
    ContextBuilder, ContextError, Focus, RefLocation, ReferenceSource, Requested, Side, render,
};
use refactor_diff_server::ai::tasks::{self, Piece};
use refactor_diff_server::exec::Tools;
use refactor_diff_server::git::Git;
use serde_json::json;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures");

fn files(dir: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read_to_string(e.path()).unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

/// The change set between a fixture's `before/` and `after/` (flat directories).
fn changes(project: &Path) -> Vec<FileChange> {
    let before = files(&project.join("before"));
    let after = files(&project.join("after"));
    let mut paths: Vec<&String> = before.iter().chain(after.iter()).map(|(p, _)| p).collect();
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .map(|p| {
            let old = before.iter().find(|(q, _)| q == p).map(|(_, t)| t.clone());
            let new = after.iter().find(|(q, _)| q == p).map(|(_, t)| t.clone());
            let status = match (&old, &new) {
                (Some(_), Some(_)) => FileStatus::Modified,
                (Some(_), None) => FileStatus::Deleted,
                _ => FileStatus::Added,
            };
            FileChange {
                path: p.clone(),
                old_path: None,
                status,
                old_text: old.unwrap_or_default(),
                new_text: new.unwrap_or_default(),
            }
        })
        .collect()
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn copy_into(src: &Path, repo: &Path) {
    for (name, text) in files(src) {
        std::fs::write(repo.join(name), text).unwrap();
    }
}

/// A fixture project as a git repo with `main` (before/) and `feature` (after/), analyzed.
struct Fixture {
    _dir: tempfile::TempDir,
    repo: PathBuf,
    report: Arc<Report>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let project = Path::new(FIXTURES).join(name);
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        copy_into(&project.join("before"), &repo);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "before"]);
        let base_sha = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["checkout", "-qb", "feature"]);
        copy_into(&project.join("after"), &repo);
        git(&repo, &["commit", "-qam", "after"]);
        let head_sha = git(&repo, &["rev-parse", "HEAD"]);
        let source = Source {
            label: "main...feature".into(),
            base: "main".into(),
            head: "feature".into(),
            base_sha,
            head_sha: Some(head_sha),
            pr: None,
            identity: "refs:main:feature".into(),
            min_count: 0,
        };
        let report = refactor_diff_core::analyze(source, changes(&project), 2, &NoHeadFiles);
        Self {
            _dir: dir,
            repo,
            report: Arc::new(report),
        }
    }

    fn git(&self) -> Git {
        Git::new(&self.repo, Arc::new(Tools::default()))
    }

    fn builder(&self) -> ContextBuilder {
        ContextBuilder::new(Arc::clone(&self.report), self.git(), None)
    }

    /// A builder over a copy of the report with its source patched.
    fn builder_with(&self, patch: impl FnOnce(&mut Source)) -> ContextBuilder {
        let mut report = (*self.report).clone();
        patch(&mut report.source);
        ContextBuilder::new(Arc::new(report), self.git(), None)
    }

    fn hunk_id(&self, path: &str) -> String {
        self.report
            .hunks
            .values()
            .find(|h| h.path == path)
            .unwrap_or_else(|| panic!("a hunk for {path}"))
            .id
            .clone()
    }
}

fn pieces(list: &[Piece]) -> BTreeSet<Piece> {
    list.iter().copied().collect()
}

fn at(f: &Focus) -> (&str, Side, u32) {
    (&f.path, f.side, f.line)
}

// --- focus ---------------------------------------------------------------------------------------

#[test]
fn focus_defaults_to_the_hunks_anchor_line() {
    let fx = Fixture::new("rename_project");
    let b = fx.builder();
    // The first unexplained change, new side: the `if` line that gained a condition.
    let focus = b.focus(&fx.hunk_id("api.py"), None, None).unwrap();
    assert_eq!(at(&focus), ("api.py", Side::New, 6));
}

#[test]
fn focus_validates_the_line() {
    let fx = Fixture::new("rename_project");
    let b = fx.builder();
    let id = fx.hunk_id("api.py");
    assert_eq!(b.focus(&id, Some("o"), Some(5)).unwrap().side, Side::Old);
    assert_eq!(b.focus(&id, Some("new"), Some(7)).unwrap().line, 7);
    assert_eq!(
        b.focus(&id, Some("n"), Some(99)).unwrap_err(),
        ContextError("Line 99 isn't part of that hunk.".into())
    );
    assert_eq!(
        b.focus(&id, Some("sideways"), Some(5)).unwrap_err(),
        ContextError("side must be old or new.".into())
    );
    assert_eq!(
        b.focus(&id, None, Some(5)).unwrap_err(),
        ContextError("side must be old or new.".into())
    );
    assert_eq!(
        b.focus("nope", None, None).unwrap_err(),
        ContextError("Unknown hunk; run the analysis again.".into())
    );
}

// --- pieces --------------------------------------------------------------------------------------

#[test]
fn hunk_piece_annotates_patterns() {
    let fx = Fixture::new("rename_project");
    let b = fx.builder();
    let focus = b.focus(&fx.hunk_id("api.py"), None, None).unwrap();
    let text = b.hunk_piece(&focus);
    assert!(
        text.contains("[[pattern: rename get_user → fetch_user (×8 in"),
        "{text}"
    );
    assert!(text.contains("[[unique replace:"), "{text}");
    // Removed and added line 5 are separate rows: the new one carries only a new number.
    assert!(
        text.contains("          5 +     user = fetch_user(user_id)"),
        "{text}"
    );
    assert!(
        text.contains("    5       -     user = get_user(user_id)"),
        "{text}"
    );
    assert_eq!(b.unexplained_lines(&focus), 2); // the `if` line, old and new
}

#[test]
fn function_piece_finds_both_sides() {
    let fx = Fixture::new("rename_project");
    let b = fx.builder();
    let focus = b.focus(&fx.hunk_id("api.py"), None, None).unwrap();
    let f = b.function_piece(&focus).unwrap();
    assert_eq!((f.qualname.as_str(), f.kind), ("handle", StmtKind::Def));
    let new = f.new.as_ref().unwrap();
    assert_eq!((new.start, new.end), (4, 8));
    assert!(new.text.contains("user.get(\"active\")"));
    let old = f.old.as_ref().unwrap();
    assert_eq!(old.start, 4);
    assert!(old.text.contains("user_id: int"));
    assert_eq!(old.path, "api.py");
    assert!(!old.truncated && !new.truncated);
}

#[test]
fn function_piece_follows_the_rename_to_the_old_side() {
    let fx = Fixture::new("rename_project");
    let b = fx.builder();
    let id = fx.hunk_id("users.py");
    let f = b
        .function_piece(&b.focus(&id, Some("n"), Some(4)).unwrap())
        .unwrap();
    assert_eq!(f.qualname, "fetch_user");
    assert!(f.old.as_ref().unwrap().text.contains("def get_user"));
    // Asked from the old side, the new counterpart is found the same way.
    let f = b
        .function_piece(&b.focus(&id, Some("o"), Some(4)).unwrap())
        .unwrap();
    assert_eq!(f.qualname, "get_user");
    assert!(f.new.as_ref().unwrap().text.contains("def fetch_user"));
}

#[test]
fn function_piece_is_none_outside_a_def() {
    let fx = Fixture::new("rename_project");
    let b = fx.builder();
    let focus = b.focus(&fx.hunk_id("api.py"), Some("n"), Some(1)).unwrap(); // the import line
    assert!(b.function_piece(&focus).is_none());
}

#[tokio::test]
async fn function_piece_for_typescript() {
    let fx = Fixture::new("ts_rename_project");
    let b = fx.builder();
    let focus = b.focus(&fx.hunk_id("api.ts"), Some("n"), Some(4)).unwrap();
    let f = b.function_piece(&focus).unwrap();
    assert_eq!(f.qualname, "handle");
    assert!(f.old.is_some() && f.new.is_some());
    let text = render(&b.collect(&focus, &pieces(&[Piece::Function])).await);
    assert!(text.contains("```ts\n"), "{text}");
}

#[test]
fn history_piece_names_the_commit_that_touched_the_line() {
    let fx = Fixture::new("rename_project");
    let b = fx.builder();
    let id = fx.hunk_id("api.py");
    let hist = b.history_piece(&b.focus(&id, Some("n"), Some(6)).unwrap());
    let subjects = |cs: &[String]| cs.to_vec();
    assert_eq!(
        subjects(
            &hist
                .range
                .iter()
                .map(|c| c.subject.clone())
                .collect::<Vec<_>>()
        ),
        ["after"]
    );
    assert_eq!(
        hist.touching
            .iter()
            .map(|c| c.subject.clone())
            .collect::<Vec<_>>(),
        ["after"]
    );
    assert_eq!(
        hist.touching[0].sha,
        fx.report.source.head_sha.clone().unwrap()
    );
    // A deleted line is found by its text.
    let hist = b.history_piece(&b.focus(&id, Some("o"), Some(6)).unwrap());
    assert_eq!(
        hist.touching
            .iter()
            .map(|c| c.subject.clone())
            .collect::<Vec<_>>(),
        ["after"]
    );
    // The working tree has no history.
    let wt = fx.builder_with(|s| s.head_sha = None);
    let hist = wt.history_piece(&wt.focus(&id, Some("n"), Some(6)).unwrap());
    assert!(hist.range.is_empty() && hist.touching.is_empty());
}

#[test]
fn pr_and_patterns_pieces() {
    let fx = Fixture::new("rename_project");
    let b = fx.builder();
    assert!(b.pr_piece().is_none());
    let with_pr = fx.builder_with(|s| {
        s.pr = Some(json!({"number": 7, "title": "Rename", "url": "u", "body": " why \n"}));
    });
    let pr = with_pr.pr_piece().unwrap();
    assert_eq!(
        (
            pr.number,
            pr.title.as_str(),
            pr.url.as_str(),
            pr.body.as_str()
        ),
        (Some(7), "Rename", "u", "why")
    );
    let pats = b.patterns_piece();
    assert!(
        pats[0].starts_with("rename get_user → fetch_user ×8 in"),
        "{pats:?}"
    );
    assert!(!pats.iter().any(|p| p.contains("insert or not"))); // unique edits aren't patterns
}

#[tokio::test]
async fn render_lays_out_every_block() {
    let fx = Fixture::new("rename_project");
    let b = fx.builder_with(|s| {
        s.pr = Some(json!({"number": 7, "title": "Rename", "url": "u", "body": "because"}));
    });
    let focus = b.focus(&fx.hunk_id("api.py"), None, None).unwrap();
    let all = pieces(&[
        Piece::Function,
        Piece::History,
        Piece::Pr,
        Piece::References,
    ]);
    let ctx = b.collect(&focus, &all).await;
    let text = render(&ctx);
    let headings: Vec<&str> = text.lines().filter(|l| l.starts_with("## ")).collect();
    assert_eq!(headings[0], "## Location");
    assert!(
        headings[1].starts_with("## Hunk (api.py @@ old line "),
        "{}",
        headings[1]
    );
    assert!(headings.contains(&"## Enclosing def handle — before (api.py:4–8)"));
    assert!(headings.contains(&"## Enclosing def handle — after (api.py:4–8)"));
    assert!(headings.contains(&"## References")); // unavailable without a navigator
    assert!(headings.contains(&"## Commits that touched this line"));
    assert!(headings.contains(&"## All commits in the range (oldest first)"));
    assert!(headings.contains(&"## Pull request #7: Rename"));
    assert_eq!(
        *headings.last().unwrap(),
        "## Mechanical patterns in this diff"
    );
    assert!(
        text.contains("```python\ndef handle(request, user_id: str):"),
        "{text}"
    );
    assert!(text.contains("because"));
    assert!(text.contains("## Location\napi.py, new side, line 6 (inside def handle)"));
    assert!(text.contains("## References\n(unavailable)"));
    assert_eq!(
        ctx.sent_pieces(&pieces(&[
            Piece::Hunk,
            Piece::Function,
            Piece::Pr,
            Piece::Patterns,
            Piece::References,
            Piece::History
        ])),
        [
            Piece::Function,
            Piece::History,
            Piece::Hunk,
            Piece::Patterns,
            Piece::Pr
        ]
    );
    assert!(matches!(ctx.references, Requested::Missing));
    // Pieces that weren't asked for don't appear at all.
    let text = render(&b.collect(&focus, &BTreeSet::new()).await);
    assert!(!text.contains("## Enclosing"));
    assert!(!text.contains("## Commits"));
    assert!(!text.contains("## Pull request"));
    assert!(!text.contains("## References"));
    assert!(text.starts_with("## Location\n"));
    assert!(
        text.contains(
            "\n\n## Mechanical patterns in this diff\nrename get_user → fetch_user ×8 in"
        )
    );
    // Without a PR, explain sends function, hunk and patterns.
    let plain = fx.builder();
    let ctx = plain
        .collect(&focus, &tasks::by_id("explain").unwrap().pieces())
        .await;
    assert_eq!(
        ctx.sent_pieces(&tasks::by_id("explain").unwrap().pieces()),
        [Piece::Function, Piece::Hunk, Piece::Patterns]
    );
    assert!(render(&ctx).contains("## Pull request\n(this comparison is not a pull request)"));
    // Outside a def, the function block says so.
    let import_line = plain
        .focus(&fx.hunk_id("api.py"), Some("n"), Some(1))
        .unwrap();
    let ctx = plain
        .collect(&import_line, &pieces(&[Piece::Function]))
        .await;
    assert!(
        render(&ctx)
            .contains("## Enclosing function\n(none: the line is not inside a def or class)")
    );
    assert_eq!(
        ctx.sent_pieces(&pieces(&[Piece::Function, Piece::Hunk])),
        [Piece::Hunk]
    );
}

// --- references ----------------------------------------------------------------------------------

type Call = (Option<String>, String, u32, u32);

struct FakeReferenceSource {
    calls: Mutex<Vec<Call>>,
    result: Result<Vec<RefLocation>, String>,
}

impl FakeReferenceSource {
    fn new(result: Result<Vec<RefLocation>, String>) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(vec![]),
            result,
        })
    }
}

impl ReferenceSource for FakeReferenceSource {
    fn references(
        &self,
        sha: Option<&str>,
        path: &str,
        line: u32,
        col: u32,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RefLocation>, String>> + Send + '_>> {
        self.calls
            .lock()
            .unwrap()
            .push((sha.map(String::from), path.to_string(), line, col));
        let result = self.result.clone();
        Box::pin(async move { result })
    }
}

fn loc(path: &str, line: u32, text: &str, def: bool) -> RefLocation {
    RefLocation {
        path: path.into(),
        line: Some(line),
        text: text.into(),
        is_definition: def,
    }
}

#[tokio::test]
async fn references_piece_uses_the_navigator() {
    let fx = Fixture::new("rename_project");
    let fake = FakeReferenceSource::new(Ok(vec![
        loc("api.py", 5, "    user = fetch_user(user_id)\n", false),
        loc("billing.py", 5, "    user = fetch_user(user_id)", false),
        loc("billing.py", 10, "    return fetch_user(user_id)", false),
        loc(
            "reports.py",
            6,
            "    users = [fetch_user(i) for i in ids]",
            false,
        ),
        loc("users.py", 4, "def fetch_user(user_id: str) -> dict:", true),
    ]));
    let b = ContextBuilder::new(Arc::clone(&fx.report), fx.git(), Some(fake.clone()));
    let focus = b
        .focus(&fx.hunk_id("users.py"), Some("n"), Some(4))
        .unwrap();
    let f = b.function_piece(&focus);
    let refs = b
        .references_piece(&focus, f.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((refs.name.as_str(), refs.side), ("fetch_user", Side::New));
    assert_eq!((refs.total, refs.files), (5, 4));
    let paths: BTreeSet<&str> = refs.locations.iter().map(|l| l.path.as_str()).collect();
    assert_eq!(
        paths,
        ["api.py", "billing.py", "reports.py", "users.py"]
            .into_iter()
            .collect()
    );
    assert!(refs.locations.iter().any(|l| l.is_definition));
    assert_eq!(refs.locations[0].text, "user = fetch_user(user_id)");
    // Looked up at head, at the name's position in `def fetch_user`.
    assert_eq!(
        fake.calls.lock().unwrap()[0],
        (fx.report.source.head_sha.clone(), "users.py".into(), 4, 5)
    );
    let text = render(
        &b.collect(&focus, &pieces(&[Piece::Function, Piece::References]))
            .await,
    );
    assert!(
        text.contains("## References to fetch_user (5 in 4 files, at head)\n"),
        "{text}"
    );
    assert!(
        text.contains("api.py:5  user = fetch_user(user_id)\n"),
        "{text}"
    );
    assert!(text.contains("users.py:4  def fetch_user(user_id: str) -> dict:   [definition]"));
    assert!(b.references_available(&focus).is_none());
    assert_eq!(b.refs_count(&focus).await, Some(5));
    assert_eq!(
        b.refs_count(&b.focus(&fx.hunk_id("api.py"), Some("n"), Some(1)).unwrap())
            .await,
        None
    );

    // The menu allows the reference tasks now.
    let rows = tasks::menu(&b, &focus);
    let uses = rows.iter().find(|r| r.id == "uses").unwrap();
    assert!(uses.unavailable.is_none());
}

#[tokio::test]
async fn references_piece_caps_the_list_and_reports_errors() {
    let fx = Fixture::new("rename_project");
    let many: Vec<RefLocation> = (0..60)
        .map(|i| loc(&format!("f{}.py", i % 7), i, "fetch_user()", false))
        .collect();
    let fake = FakeReferenceSource::new(Ok(many));
    let b = ContextBuilder::new(Arc::clone(&fx.report), fx.git(), Some(fake));
    let focus = b
        .focus(&fx.hunk_id("users.py"), Some("n"), Some(4))
        .unwrap();
    let ctx = b.collect(&focus, &pieces(&[Piece::References])).await;
    let refs = ctx.references.as_option().unwrap().as_ref().unwrap();
    assert_eq!((refs.total, refs.files, refs.locations.len()), (60, 7, 50));
    let text = render(&ctx);
    assert!(text.contains("… and 10 more"));
    assert!(!text.contains("## Enclosing")); // the function wasn't asked for
    assert_eq!(
        ctx.sent_pieces(&pieces(&[Piece::References, Piece::Hunk])),
        [Piece::Hunk, Piece::References]
    );

    let fake = FakeReferenceSource::new(Err("jedi crashed".into()));
    let b = ContextBuilder::new(Arc::clone(&fx.report), fx.git(), Some(fake.clone()));
    let focus = b
        .focus(&fx.hunk_id("users.py"), Some("n"), Some(4))
        .unwrap();
    let ctx = b.collect(&focus, &pieces(&[Piece::References])).await;
    let err = ctx.references.as_option().unwrap().as_ref().unwrap_err();
    assert_eq!(
        (err.name.as_str(), err.side, err.error.as_str()),
        ("fetch_user", Side::New, "jedi crashed")
    );
    assert!(render(&ctx).contains("## References to fetch_user\n(unavailable: jedi crashed)"));
    assert_eq!(
        ctx.sent_pieces(&pieces(&[Piece::References])),
        [Piece::References]
    );
    assert_eq!(b.refs_count(&focus).await, None);

    // From the old side of a renamed def the name isn't in the new-side span (as in Python):
    // no lookup happens and the block says the references are unavailable.
    let focus = b
        .focus(&fx.hunk_id("users.py"), Some("o"), Some(4))
        .unwrap();
    let calls_before = fake.calls.lock().unwrap().len();
    let ctx = b.collect(&focus, &pieces(&[Piece::References])).await;
    assert!(matches!(ctx.references, Requested::Missing));
    assert_eq!(fake.calls.lock().unwrap().len(), calls_before);
    assert!(render(&ctx).contains("## References\n(unavailable)"));
}

#[tokio::test]
async fn references_unavailable_without_navigation() {
    let fx = Fixture::new("rename_project");
    let b = fx.builder();
    let focus = b.focus(&fx.hunk_id("api.py"), None, None).unwrap();
    assert_eq!(
        b.references_available(&focus).as_deref(),
        Some("code navigation is off")
    );
    let f = b.function_piece(&focus);
    assert!(b.references_piece(&focus, f.as_ref()).await.is_none());
    assert_eq!(b.refs_count(&focus).await, None);
}

// --- tasks ---------------------------------------------------------------------------------------

#[test]
fn menu_rows_carry_hints_and_availability() {
    let fx = Fixture::new("rename_project");
    let b = fx.builder();
    let focus = b.focus(&fx.hunk_id("api.py"), None, None).unwrap();
    let rows = tasks::menu(&b, &focus);
    let ids: Vec<&str> = rows.iter().map(|r| r.id).collect();
    assert_eq!(
        ids,
        [
            "explain",
            "function",
            "compare",
            "uses",
            "why",
            "review",
            "preserving",
            "break"
        ]
    );
    let row = |id: &str| rows.iter().find(|r| r.id == id).unwrap();
    assert_eq!(row("explain").hint, "2 unexplained lines");
    assert!(row("explain").unavailable.is_none());
    assert_eq!(row("function").hint, "handle()");
    assert_eq!(
        row("uses").unavailable.as_deref(),
        Some("code navigation is off")
    );
    assert_eq!(row("why").hint, "1 commit");
    assert!(row("why").unavailable.is_none());
    assert_eq!(row("preserving").hint, "not verified");
    assert_eq!(row("review").group, "Review and risk");
    assert_eq!(
        serde_json::to_value(row("compare")).unwrap(),
        json!({"id": "compare", "label": "Compare old vs new", "group": "Understand", "hint": "handle()", "unavailable": null})
    );
}

#[test]
fn menu_outside_a_def_and_on_the_working_tree() {
    let fx = Fixture::new("rename_project");
    let b = fx.builder();
    let focus = b.focus(&fx.hunk_id("api.py"), Some("n"), Some(1)).unwrap();
    let rows = tasks::menu(&b, &focus);
    let row = |id: &str| rows.iter().find(|r| r.id == id).unwrap().clone();
    assert_eq!(
        row("function").unavailable.as_deref(),
        Some("the line isn't inside a function or class")
    );
    assert_eq!(
        row("compare").unavailable.as_deref(),
        Some("the line isn't inside a function or class")
    );
    assert_eq!(row("function").hint, "");
    let wt = fx.builder_with(|s| s.head_sha = None);
    let rows = tasks::menu(&wt, &focus);
    let why = rows.iter().find(|r| r.id == "why").unwrap();
    assert_eq!(
        why.unavailable.as_deref(),
        Some("no commits or pull request for the working tree")
    );
    assert_eq!(why.hint, "");
    // A PR makes `why` available again and shows up in the hint.
    let pr = fx.builder_with(|s| {
        s.head_sha = None;
        s.pr = Some(json!({"number": 12, "title": "t", "url": "u", "body": ""}));
    });
    let rows = tasks::menu(&pr, &focus);
    let why = rows.iter().find(|r| r.id == "why").unwrap();
    assert!(why.unavailable.is_none());
    assert_eq!(why.hint, "PR #12");
    let both = fx.builder_with(|s| {
        s.pr = Some(json!({"number": 12, "title": "t", "url": "u", "body": ""}));
    });
    let rows = tasks::menu(&both, &focus);
    assert_eq!(
        rows.iter().find(|r| r.id == "why").unwrap().hint,
        "PR #12 · 1 commit"
    );
}

#[test]
fn compare_needs_both_sides() {
    let fx = Fixture::new("rename_project");
    let b = fx.builder();
    // `fetch_user` exists on both sides via the rename, so compare is available there.
    let focus = b
        .focus(&fx.hunk_id("users.py"), Some("n"), Some(4))
        .unwrap();
    let f = b.function_piece(&focus);
    let compare = tasks::by_id("compare").unwrap();
    assert!(compare.unavailable(&b, &focus, f.as_ref()).is_none());
    assert_eq!(compare.hint(&b, &focus, f.as_ref()), "fetch_user()");
    // Simulate a def that only exists on one side.
    let mut one_sided = f.clone().unwrap();
    one_sided.old = None;
    assert_eq!(
        compare.unavailable(&b, &focus, Some(&one_sided)).as_deref(),
        Some("fetch_user is new in this diff")
    );
    one_sided.old = f.clone().unwrap().old;
    one_sided.new = None;
    assert_eq!(
        compare.unavailable(&b, &focus, Some(&one_sided)).as_deref(),
        Some("fetch_user was removed in this diff")
    );
}
