//! TypeScript navigation through tsserver.
//!
//! The protocol, process handling and result mapping are exercised against a small fake
//! tsserver (a Python script speaking the `Content-Length` framing), which only needs
//! `python3`. The tests against a real tsserver are skipped unless one is available: set
//! `TSSERVER` to a tsserver executable or TypeScript's `lib/tsserver.js`, put `tsserver` on
//! PATH, or install TypeScript globally.

mod nav_support;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nav_support::{Fixture, fixture, node, python3, tsserver, where_};
use refactor_diff_server::exec::Tools;
use refactor_diff_server::nav::{Kind, Location, Navigator};

// --- a real tsserver -------------------------------------------------------------------------

struct Nav {
    navigator: Navigator,
    fixture: Fixture,
}

fn real_nav() -> Option<Nav> {
    let (Some(tsserver), Some(_node)) = (tsserver(), node()) else {
        eprintln!(
            "note: no tsserver (set TSSERVER or install TypeScript) or Node.js; skipping real \
             tsserver test"
        );
        return None;
    };
    let fixture = fixture("ts_rename_project");
    let navigator = Navigator::new(
        fixture.repo.clone(),
        Arc::clone(&fixture.snapshots),
        Arc::clone(&fixture.tools),
        None,
        Some(tsserver),
    );
    Some(Nav { navigator, fixture })
}

#[tokio::test]
async fn definition_on_new_side() {
    let Some(nav) = real_nav() else { return };
    // api.ts line 4: "  const user = fetchUser(userId);"
    let locs = nav
        .navigator
        .definitions(Some(&nav.fixture.head), "api.ts", 4, 16)
        .await
        .unwrap();
    assert_eq!(locs.len(), 1, "{locs:?}");
    let loc = &locs[0];
    assert_eq!(
        (
            loc.kind,
            loc.path.as_str(),
            loc.line,
            loc.col,
            loc.name.as_str()
        ),
        (Kind::Repo, "users.ts", Some(3), Some(16), "fetchUser")
    );
    nav.navigator.close().await;
}

#[tokio::test]
async fn definition_on_old_side_uses_base_revision() {
    let Some(nav) = real_nav() else { return };
    let locs = nav
        .navigator
        .definitions(Some(&nav.fixture.base), "api.ts", 4, 16)
        .await
        .unwrap();
    assert_eq!(locs.len(), 1, "{locs:?}");
    let loc = &locs[0];
    assert_eq!(
        (loc.path.as_str(), loc.line, loc.name.as_str()),
        ("users.ts", Some(3), "getUser")
    );
    nav.navigator.close().await;
}

#[tokio::test]
async fn references_follow_the_revision() {
    let Some(nav) = real_nav() else { return };
    let files = |locs: &[Location]| {
        locs.iter()
            .map(|l| l.path.clone())
            .collect::<BTreeSet<String>>()
    };
    let new_refs = nav
        .navigator
        .references(Some(&nav.fixture.head), "users.ts", 3, 16)
        .await
        .unwrap();
    let old_refs = nav
        .navigator
        .references(Some(&nav.fixture.base), "users.ts", 3, 16)
        .await
        .unwrap();
    let expected: BTreeSet<String> = ["api.ts", "billing.ts", "reports.ts", "users.ts"]
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(files(&new_refs), expected);
    let mut with_legacy = expected.clone();
    with_legacy.insert("legacy.js".into()); // legacy.js still calls getUser
    assert_eq!(files(&old_refs), with_legacy);
    let definitions: Vec<&Location> = new_refs.iter().filter(|l| l.is_definition).collect();
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].path, "users.ts");
    nav.navigator.close().await;
}

#[tokio::test]
async fn builtin_resolves_to_a_lib_file() {
    let Some(nav) = real_nav() else { return };
    // reports.ts line 4: "  const users = ids.map((i) => fetchUser(i));"
    let locs = nav
        .navigator
        .definitions(Some(&nav.fixture.head), "reports.ts", 4, 22)
        .await
        .unwrap();
    assert_eq!(locs.len(), 1, "{locs:?}");
    let loc = &locs[0];
    assert_eq!(loc.kind, Kind::Library);
    assert!(loc.path.ends_with(".d.ts"), "{}", loc.path);
    assert_eq!(loc.name, "map");
    assert!(
        nav.navigator
            .library_source(&loc.path)
            .unwrap()
            .contains("map<U>")
    );
    nav.navigator.close().await;
}

#[tokio::test]
async fn describe_environment() {
    let Some(nav) = real_nav() else { return };
    let env = nav.navigator.describe_environment("api.ts").await.unwrap();
    assert!(env.starts_with("TypeScript "), "{env}");
    nav.navigator.close().await;
}

#[tokio::test]
async fn missing_file_is_an_error() {
    let Some(nav) = real_nav() else { return };
    let err = nav
        .navigator
        .definitions(Some(&nav.fixture.base), "nope.ts", 1, 0)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "nope.ts doesn't exist at this revision.");
}

#[tokio::test]
async fn working_tree() {
    let Some(nav) = real_nav() else { return };
    let locs = nav
        .navigator
        .definitions(None, "billing.ts", 4, 16)
        .await
        .unwrap();
    assert_eq!(
        where_(&locs),
        vec![(Kind::Repo, "users.ts".into(), Some(3))]
    );
    nav.navigator.close().await;
}

// --- discovery -------------------------------------------------------------------------------

/// Tools with an empty PATH: nothing resolves, whatever the machine has installed.
fn no_tools() -> Arc<Tools> {
    Arc::new(Tools::new(Some(OsString::new())))
}

#[tokio::test]
async fn explicit_but_missing_tsserver_is_an_error() {
    let fixture = fixture("ts_rename_project");
    let navigator = Navigator::new(
        fixture.repo.clone(),
        Arc::clone(&fixture.snapshots),
        no_tools(),
        None,
        Some(PathBuf::from("/nonexistent/tsserver")),
    );
    let err = navigator
        .definitions(None, "api.ts", 4, 16)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Can't find tsserver at /nonexistent/tsserver."
    );
}

#[tokio::test]
async fn no_tsserver_anywhere_gives_the_install_hint() {
    let fixture = fixture("ts_rename_project");
    let navigator = Navigator::new(
        fixture.repo.clone(),
        Arc::clone(&fixture.snapshots),
        no_tools(),
        None,
        None,
    );
    let err = navigator
        .definitions(None, "api.ts", 4, 16)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "TypeScript navigation needs tsserver: install TypeScript in the repository (npm \
         install --save-dev typescript) or pass --tsserver PATH."
    );
    // The environment description fails the same way.
    assert_eq!(
        navigator.describe_environment("api.ts").await.unwrap_err(),
        err
    );
}

#[tokio::test]
async fn tsserver_js_needs_node() {
    let fixture = fixture("ts_rename_project");
    let script = fixture.repo.join("node_modules/typescript/lib/tsserver.js");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&script, "").unwrap();
    let navigator = Navigator::new(
        fixture.repo.clone(),
        Arc::clone(&fixture.snapshots),
        no_tools(),
        None,
        None,
    );
    let err = navigator
        .definitions(None, "api.ts", 4, 16)
        .await
        .unwrap_err();
    // With an empty PATH only an nvm install can supply node; node then runs the empty
    // script, which exits at once.
    if node().is_none() {
        assert_eq!(
            err.to_string(),
            "TypeScript navigation needs Node.js on PATH."
        );
    } else {
        assert!(
            err.to_string().starts_with("tsserver stopped responding:")
                || err.to_string().starts_with("Can't start tsserver:"),
            "{err}"
        );
    }
}

// --- a fake tsserver -------------------------------------------------------------------------

const FAKE_TSSERVER: &str = r#"#!/usr/bin/env python3
"""A stand-in tsserver: Content-Length framing, events before responses, canned answers."""
import json
import os
import re
import sys

seq = 0


def send(obj):
    body = json.dumps(obj)
    sys.stdout.write("Content-Length: %d\r\n\r\n%s\n" % (len(body.encode()) + 1, body))
    sys.stdout.flush()


for raw in sys.stdin:
    req = json.loads(raw)
    cmd, args = req["command"], req.get("arguments", {})
    if cmd == "exit":
        sys.exit(0)
    if cmd in ("open", "close"):
        continue
    seq += 1
    send({"seq": seq, "type": "event", "event": "projectLoadingStart", "body": {}})
    seq += 1
    root = os.path.dirname(args["file"])
    line = args.get("line")
    if line == 99:
        send({"seq": seq, "type": "response", "request_seq": req["seq"], "command": cmd,
              "success": False, "message": "No content available."})
        continue
    if line == 77:
        sys.exit(3)  # crash mid-request
    users = os.path.join(root, "users.ts")
    with open(users) as f:
        third = f.read().split("\n")[2]
    m = re.search(r"function (\w+)", third)  # the definition's span, 1-based offsets
    start, end = m.start(1) + 1, m.end(1) + 1
    if cmd == "definition":
        body = [{"file": users, "start": {"line": 3, "offset": start}, "end": {"line": 3, "offset": end}}]
    else:
        body = {"symbolName": "fetchUser", "refs": [
            {"file": os.path.join(root, "api.ts"), "start": {"line": 4, "offset": 16},
             "end": {"line": 4, "offset": 25}, "lineText": "  const user = fetchUser(userId);",
             "isDefinition": False, "isWriteAccess": False},
            {"file": users, "start": {"line": 3, "offset": start}, "end": {"line": 3, "offset": end},
             "lineText": third, "isDefinition": True, "isWriteAccess": True},
            {"file": os.path.join(root, "node_modules", "dep", "index.d.ts"),
             "start": {"line": 1, "offset": 18}, "end": {"line": 1, "offset": 27},
             "lineText": "export declare x: fetchUser;", "isDefinition": False, "isWriteAccess": False},
        ]}
    send({"seq": seq, "type": "response", "request_seq": req["seq"], "command": cmd,
          "success": True, "body": body})
"#;

struct FakeNav {
    navigator: Navigator,
    fixture: Fixture,
    script: PathBuf,
}

fn fake_nav() -> Option<FakeNav> {
    if python3().is_none() {
        eprintln!("note: no python3 on PATH; skipping fake tsserver test");
        return None;
    }
    let fixture = fixture("ts_rename_project");
    let script = fixture.repo.join("fake-tsserver");
    std::fs::write(&script, FAKE_TSSERVER).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let dep = fixture.repo.join("node_modules/dep");
    std::fs::create_dir_all(&dep).unwrap();
    std::fs::write(dep.join("index.d.ts"), "export declare x: fetchUser;\n").unwrap();
    let navigator = Navigator::new(
        fixture.repo.clone(),
        Arc::clone(&fixture.snapshots),
        Arc::clone(&fixture.tools),
        None,
        Some(script.clone()),
    );
    Some(FakeNav {
        navigator,
        fixture,
        script,
    })
}

#[tokio::test]
async fn fake_definition_maps_offsets_and_names() {
    let Some(nav) = fake_nav() else { return };
    let locs = nav
        .navigator
        .definitions(None, "api.ts", 4, 16)
        .await
        .unwrap();
    assert_eq!(locs.len(), 1, "{locs:?}");
    let loc = &locs[0];
    assert_eq!(loc.kind, Kind::Repo);
    assert_eq!(loc.path, "users.ts");
    assert_eq!((loc.line, loc.col), (Some(3), Some(16)));
    assert_eq!(
        loc.name, "fetchUser",
        "sliced from the line when tsserver gives none"
    );
    assert_eq!(
        loc.text,
        "export function fetchUser(userId: string): object {"
    );
    assert_eq!(loc.kind_name, "definition");
    assert!(loc.is_definition);
    nav.navigator.close().await;
}

#[tokio::test]
async fn fake_references_split_repo_and_library() {
    let Some(nav) = fake_nav() else { return };
    let locs = nav
        .navigator
        .references(None, "users.ts", 3, 16)
        .await
        .unwrap();
    let got = where_(&locs);
    let lib = nav.fixture.repo.join("node_modules/dep/index.d.ts");
    assert_eq!(
        got,
        vec![
            (Kind::Repo, "api.ts".into(), Some(4)),
            (Kind::Repo, "users.ts".into(), Some(3)),
            (Kind::Library, lib.to_string_lossy().into_owned(), Some(1)),
        ],
        "repository files first, node_modules is a library"
    );
    assert!(locs.iter().all(|l| l.name == "fetchUser"));
    let definitions: Vec<&Location> = locs.iter().filter(|l| l.is_definition).collect();
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].path, "users.ts");
    assert_eq!(definitions[0].kind_name, "definition");
    assert_eq!(locs[0].kind_name, "reference");
    assert_eq!(locs[0].text, "  const user = fetchUser(userId);");
    assert_eq!(locs[2].col, Some(17));
    assert!(
        nav.navigator
            .library_source(&locs[2].path)
            .unwrap()
            .contains("declare x")
    );
    nav.navigator.close().await;
}

#[tokio::test]
async fn fake_unsuccessful_response_is_empty() {
    let Some(nav) = fake_nav() else { return };
    // A fake-specific signal: line 99 of a padded file answers success: false.
    let padded = nav.fixture.repo.join("padded.ts");
    std::fs::write(&padded, "x\n".repeat(120)).unwrap();
    let locs = nav
        .navigator
        .definitions(None, "padded.ts", 99, 0)
        .await
        .unwrap();
    assert!(locs.is_empty());
    nav.navigator.close().await;
}

#[tokio::test]
async fn fake_line_outside_file_is_an_error() {
    let Some(nav) = fake_nav() else { return };
    let err = nav
        .navigator
        .definitions(None, "users.ts", 50, 0)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "Line 50 is outside users.ts.");
    let err = nav
        .navigator
        .definitions(None, "nope.ts", 1, 0)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "nope.ts doesn't exist at this revision.");
}

#[tokio::test]
async fn fake_crash_is_reported_and_the_server_restarts() {
    let Some(nav) = fake_nav() else { return };
    let padded = nav.fixture.repo.join("padded.ts");
    std::fs::write(&padded, "x\n".repeat(120)).unwrap();
    let err = nav
        .navigator
        .definitions(None, "padded.ts", 77, 0)
        .await
        .unwrap_err();
    assert!(
        err.to_string().starts_with("tsserver stopped responding:"),
        "{err}"
    );
    let locs = nav
        .navigator
        .definitions(None, "api.ts", 4, 16)
        .await
        .unwrap();
    assert_eq!(
        where_(&locs),
        vec![(Kind::Repo, "users.ts".into(), Some(3))]
    );
    nav.navigator.close().await;
}

#[tokio::test]
async fn fake_describe_environment_without_a_package_manifest() {
    let Some(nav) = fake_nav() else { return };
    let env = nav.navigator.describe_environment("api.ts").await.unwrap();
    let script = nav.script.canonicalize().unwrap();
    // The repository's own package.json has no version, so no version is shown.
    assert_eq!(env, format!("TypeScript at {}", script.display()));

    let manifest = nav.fixture.repo.join("package.json");
    std::fs::write(&manifest, r#"{"name": "x", "version": "9.9.9"}"#).unwrap();
    let fresh = Navigator::new(
        nav.fixture.repo.clone(),
        Arc::clone(&nav.fixture.snapshots),
        Arc::clone(&nav.fixture.tools),
        None,
        Some(nav.script.clone()),
    );
    assert_eq!(
        fresh.describe_environment("api.ts").await.unwrap(),
        format!("TypeScript 9.9.9 at {}", script.display())
    );
}

#[tokio::test]
async fn fake_snapshot_side_resolves_against_the_snapshot() {
    let Some(nav) = fake_nav() else { return };
    let locs = nav
        .navigator
        .definitions(Some(&nav.fixture.base), "api.ts", 4, 16)
        .await
        .unwrap();
    assert_eq!(
        where_(&locs),
        vec![(Kind::Repo, "users.ts".into(), Some(3))]
    );
    // The text comes from the base revision's file, where the function is still getUser.
    assert_eq!(
        locs[0].text,
        "export function getUser(userId: number): object {"
    );
    assert_eq!(locs[0].name, "getUser");
    assert!(!Path::new(&locs[0].path).is_absolute());
    nav.navigator.close().await;
}
