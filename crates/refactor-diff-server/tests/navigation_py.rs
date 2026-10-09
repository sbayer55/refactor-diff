//! Python navigation through the bundled Jedi helper. Needs a `python3` (3.10+) on PATH;
//! skipped with a note otherwise.

mod nav_support;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use nav_support::{Fixture, fixture, git, python3, where_};
use refactor_diff_server::exec::Tools;
use refactor_diff_server::git::Git;
use refactor_diff_server::nav::{Kind, Navigator};
use refactor_diff_server::snapshots::Snapshots;

struct Nav {
    navigator: Navigator,
    fixture: Fixture,
    _cache: tempfile::TempDir,
}

fn nav() -> Option<Nav> {
    let Some(python) = python3() else {
        eprintln!("note: no python3 >= 3.10 on PATH; skipping Python navigation test");
        return None;
    };
    let fixture = fixture("rename_project");
    let cache = tempfile::tempdir().unwrap();
    let navigator = Navigator::new(
        fixture.repo.clone(),
        Arc::clone(&fixture.snapshots),
        Arc::clone(&fixture.tools),
        Some(python),
        None,
    )
    .with_cache_dir(cache.path());
    Some(Nav {
        navigator,
        fixture,
        _cache: cache,
    })
}

#[tokio::test]
async fn definition_on_new_side() {
    let Some(nav) = nav() else { return };
    // api.py line 5: "    user = fetch_user(user_id)"
    let locs = nav
        .navigator
        .definitions(Some(&nav.fixture.head), "api.py", 5, 12)
        .await
        .unwrap();
    assert_eq!(
        where_(&locs),
        vec![(Kind::Repo, "users.py".into(), Some(4))]
    );
    assert_eq!(locs[0].name, "fetch_user");
    assert_eq!(locs[0].text, "def fetch_user(user_id: str) -> dict:");
    assert_eq!(locs[0].kind_name, "function");
    assert!(locs[0].is_definition);
    nav.navigator.close().await;
}

#[tokio::test]
async fn definition_on_old_side_uses_base_revision() {
    let Some(nav) = nav() else { return };
    // api.py line 5 at base: "    user = get_user(user_id)"
    let locs = nav
        .navigator
        .definitions(Some(&nav.fixture.base), "api.py", 5, 12)
        .await
        .unwrap();
    assert_eq!(
        where_(&locs),
        vec![(Kind::Repo, "users.py".into(), Some(4))]
    );
    assert_eq!(locs[0].name, "get_user");
    nav.navigator.close().await;
}

#[tokio::test]
async fn references_follow_the_revision() {
    let Some(nav) = nav() else { return };
    let files = |locs: Vec<refactor_diff_server::nav::Location>| {
        locs.into_iter().map(|l| l.path).collect::<BTreeSet<_>>()
    };
    let new_files = files(
        nav.navigator
            .references(Some(&nav.fixture.head), "users.py", 4, 4)
            .await
            .unwrap(),
    );
    let old_files = files(
        nav.navigator
            .references(Some(&nav.fixture.base), "users.py", 4, 4)
            .await
            .unwrap(),
    );
    let expected: BTreeSet<String> = ["api.py", "billing.py", "reports.py", "users.py"]
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(new_files, expected);
    let mut with_legacy = expected.clone();
    with_legacy.insert("legacy.py".into()); // legacy.py still calls get_user
    assert_eq!(old_files, with_legacy);
    nav.navigator.close().await;
}

#[tokio::test]
async fn builtin_resolves_to_a_stub() {
    let Some(nav) = nav() else { return };
    // reports.py line 7: '    return {"count": len(users), ...'
    let locs = nav
        .navigator
        .definitions(Some(&nav.fixture.head), "reports.py", 7, 24)
        .await
        .unwrap();
    assert_eq!(locs.len(), 1, "{locs:?}");
    let loc = &locs[0];
    assert_eq!(loc.kind, Kind::Library);
    assert!(loc.path.ends_with("builtins.pyi"), "{}", loc.path);
    assert_eq!(loc.name, "len");
    assert!(loc.text.contains("def len"), "{}", loc.text);
    assert!(
        nav.navigator
            .library_source(&loc.path)
            .unwrap()
            .contains("def len")
    );
    nav.navigator.close().await;
}

#[tokio::test]
async fn library_source_only_for_navigated_files() {
    let Some(nav) = nav() else { return };
    let err = nav.navigator.library_source("/etc/hosts").unwrap_err();
    assert_eq!(
        err.to_string(),
        "Only library files reached through navigation can be viewed."
    );
}

#[tokio::test]
async fn missing_file_is_an_error() {
    let Some(nav) = nav() else { return };
    let err = nav
        .navigator
        .definitions(Some(&nav.fixture.base), "nope.py", 1, 0)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "nope.py doesn't exist at this revision.");
}

#[tokio::test]
async fn unsupported_file_is_an_error() {
    let Some(nav) = nav() else { return };
    let err = nav
        .navigator
        .definitions(Some(&nav.fixture.base), "README.txt", 1, 0)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Code navigation isn't available for README.txt."
    );
}

#[tokio::test]
async fn position_outside_the_file_is_an_error() {
    let Some(nav) = nav() else { return };
    let err = nav
        .navigator
        .definitions(Some(&nav.fixture.head), "api.py", 99, 0)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("line"), "{err}");
    // The helper survives a bad query.
    let locs = nav
        .navigator
        .definitions(Some(&nav.fixture.head), "api.py", 5, 12)
        .await
        .unwrap();
    assert_eq!(
        where_(&locs),
        vec![(Kind::Repo, "users.py".into(), Some(4))]
    );
    nav.navigator.close().await;
}

#[tokio::test]
async fn describe_environment_names_the_interpreter() {
    let Some(nav) = nav() else { return };
    let env = nav.navigator.describe_environment("api.py").await.unwrap();
    assert!(env.starts_with("Python 3."), "{env}");
    assert!(env.contains(" at "), "{env}");
    nav.navigator.close().await;
}

#[tokio::test]
async fn working_tree_query() {
    let Some(nav) = nav() else { return };
    let locs = nav
        .navigator
        .definitions(None, "billing.py", 5, 12)
        .await
        .unwrap();
    assert_eq!(
        where_(&locs),
        vec![(Kind::Repo, "users.py".into(), Some(4))]
    );
    nav.navigator.close().await;
}

#[tokio::test]
async fn bad_interpreter_is_reported() {
    let Some(_) = python3() else { return };
    let fixture = fixture("rename_project");
    let cache = tempfile::tempdir().unwrap();
    let navigator = Navigator::new(
        fixture.repo.clone(),
        Arc::clone(&fixture.snapshots),
        Arc::clone(&fixture.tools),
        Some(PathBuf::from("/nonexistent/python")),
        None,
    )
    .with_cache_dir(cache.path());
    let err = navigator
        .definitions(Some(&fixture.head), "api.py", 5, 12)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .starts_with("Can't use Python environment /nonexistent/python:"),
        "{err}"
    );
}

/// The venv's editable install points at the checkout; navigation at head must still land in
/// the head revision, not the (older) checked-out file.
#[tokio::test]
async fn editable_install_resolves_into_the_snapshot() {
    let Some(python) = python3() else {
        eprintln!("note: no python3 >= 3.10 on PATH; skipping Python navigation test");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("proj");
    std::fs::create_dir_all(repo.join("src/pkg")).unwrap();
    std::fs::write(repo.join("src/pkg/__init__.py"), "").unwrap();
    std::fs::write(repo.join("src/pkg/mod.py"), "def f():\n    pass\n").unwrap();
    std::fs::write(
        repo.join("src/pkg/use.py"),
        "from pkg.mod import f\n\nf()\n",
    )
    .unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    std::fs::write(
        repo.join("src/pkg/mod.py"),
        "X = 1\n\n\ndef f():\n    pass\n",
    )
    .unwrap();
    git(&repo, &["commit", "-qam", "move f"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["checkout", "-q", "HEAD~1"]); // the checkout now has f() on line 1

    let status = Command::new(&python)
        .args(["-m", "venv", "--without-pip"])
        .arg(repo.join(".venv"))
        .status()
        .unwrap();
    assert!(status.success());
    let site = std::fs::read_dir(repo.join(".venv/lib"))
        .unwrap()
        .flatten()
        .map(|e| e.path().join("site-packages"))
        .find(|p| p.is_dir())
        .expect("site-packages");
    std::fs::write(
        site.join("_pkg_editable.pth"),
        format!("{}\n", repo.join("src").display()),
    )
    .unwrap();

    let tools = Arc::new(Tools::default());
    let snapshots = Arc::new(
        Snapshots::new(
            Git::new(repo.clone(), Arc::clone(&tools)),
            Some(dir.path().join("snaps")),
        )
        .unwrap(),
    );
    let cache = tempfile::tempdir().unwrap();
    // No interpreter given: the navigator finds .venv on its own.
    let navigator =
        Navigator::new(repo.clone(), snapshots, tools, None, None).with_cache_dir(cache.path());
    let locs = navigator
        .definitions(Some(&head), "src/pkg/use.py", 3, 0)
        .await
        .unwrap();
    assert_eq!(
        where_(&locs),
        vec![(Kind::Repo, "src/pkg/mod.py".into(), Some(4))]
    );
    let env = navigator
        .describe_environment("src/pkg/use.py")
        .await
        .unwrap();
    assert!(env.starts_with("Python 3."), "{env}");
    let prefix = Path::new(env.rsplit(" at ").next().unwrap());
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    assert!(
        canonical(prefix).starts_with(canonical(&repo.join(".venv"))),
        "{env}"
    );
    navigator.close().await;
}
