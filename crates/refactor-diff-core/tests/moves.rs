//! Port of tests/test_moves.py: blocks deleted in one place and inserted in another.

mod common;

use std::collections::{BTreeMap, HashSet};

use common::*;
use refactor_diff_core::{FileStatus, Group, Report, SignatureKind, Unit, analyze};

const HELPER: &str = "def helper(x, y):
    \"\"\"Add things up.\"\"\"
    total = x + y
    if total > 10:
        return total * 2
    return total
";

const OTHER: &str = "def other(a):
    b = a - 1
    return b * b
";

/// `analyze(repo, "main", "feature")` over an in-memory repository: `after` is the whole head
/// tree (a file missing from it was deleted). Files whose content did not change are left out,
/// as `git diff` would.
fn report_for(before: &[(&str, &str)], after: &[(&str, &str)]) -> Report {
    let mut changes = changes_from(before, after);
    changes.retain(|c| c.status != FileStatus::Modified || c.old_text != c.new_text);
    let head = DirHeadFiles::from_files(after);
    analyze(source("main", "feature"), changes, 2, &head)
}

fn single<T>(items: Vec<T>) -> T {
    assert_eq!(items.len(), 1, "expected exactly one item");
    items.into_iter().next().unwrap()
}

fn move_groups(report: &Report) -> Vec<&Group> {
    report
        .groups
        .iter()
        .filter(|g| g.kind == SignatureKind::Move)
        .collect()
}

fn details(group: &Group) -> BTreeMap<&str, usize> {
    group
        .details
        .iter()
        .map(|(k, v)| (k.as_str(), *v))
        .collect()
}

fn residual(report: &Report) -> Vec<&Unit> {
    report.units.values().filter(|u| !u.explained).collect()
}

#[test]
fn exact_move_between_files() {
    let report = report_for(
        &[
            ("a.py", &format!("{HELPER}\n\n{OTHER}")),
            ("b.py", "X = 1\n"),
        ],
        &[("a.py", OTHER), ("b.py", &format!("X = 1\n\n\n{HELPER}"))],
    );
    let mv = single(move_groups(&report));
    assert!(mv.mechanical);
    assert_eq!(mv.label, "moved helper: a.py → b.py");
    assert_eq!(mv.unit_ids.len(), 2);
    assert_eq!(report.stats().residual_units, 0);
    let old = &report.units[&mv.unit_ids[0]];
    let new = &report.units[&mv.unit_ids[1]];
    assert_eq!(old.partner.as_deref(), Some(new.id.as_str()));
    assert_eq!(new.partner.as_deref(), Some(old.id.as_str()));
    assert!(old.verified && new.verified);
}

#[test]
fn move_with_edit_inside_leaves_only_the_edit() {
    let edited = HELPER.replace("total * 2", "total * 3");
    let report = report_for(
        &[
            ("a.py", &format!("{HELPER}\n\n{OTHER}")),
            ("b.py", "X = 1\n"),
        ],
        &[("a.py", OTHER), ("b.py", &format!("X = 1\n\n\n{edited}"))],
    );
    let _mv = single(move_groups(&report));
    let residual = residual(&report);
    assert_eq!(
        residual.iter().map(|u| u.path.as_str()).collect::<Vec<_>>(),
        vec!["b.py"]
    );
    let new = single(residual);
    let kinds: HashSet<SignatureKind> = new.signature_kinds().collect();
    assert_eq!(
        kinds,
        HashSet::from([SignatureKind::Move, SignatureKind::Replace])
    );
    // Only the changed token is highlighted on the moved block.
    let marked: Vec<String> = new
        .new
        .iter()
        .flat_map(|ln| {
            ln.hl.iter().map(|[s, e]| {
                ln.text
                    .chars()
                    .skip(*s as usize)
                    .take((*e - *s) as usize)
                    .collect::<String>()
            })
        })
        .collect();
    assert_eq!(marked, vec!["3"]);
    assert!(!new.verified);
}

#[test]
fn move_with_rename_inside_joins_the_rename_group() {
    let renamed = HELPER.replace("total", "amount");
    let report = report_for(
        &[
            (
                "a.py",
                &format!("{HELPER}\n\n{OTHER}\n\ntotal = 1\nprint(total)\n"),
            ),
            ("b.py", ""),
        ],
        &[
            ("a.py", &format!("{OTHER}\n\namount = 1\nprint(amount)\n")),
            ("b.py", &renamed),
        ],
    );
    let _mv = single(move_groups(&report));
    let rename = single(
        report
            .groups
            .iter()
            .filter(|g| g.kind == SignatureKind::Rename)
            .collect(),
    );
    assert_eq!(
        (rename.old.as_str(), rename.new.as_str()),
        ("total", "amount")
    );
    assert!(rename.mechanical);
    assert_eq!(report.stats().residual_units, 0);
}

#[test]
fn tiny_block_is_not_a_move() {
    let report = report_for(
        &[(
            "a.py",
            "def f():\n    return None\n\n\ndef g():\n    return 1\n",
        )],
        &[(
            "a.py",
            "def f():\n    return 2\n\n\ndef g():\n    return None\n",
        )],
    );
    assert!(move_groups(&report).is_empty());
}

#[test]
fn same_file_reorder() {
    let report = report_for(
        &[("a.py", &format!("{HELPER}\n\n{OTHER}"))],
        &[("a.py", &format!("{OTHER}\n\n{HELPER}"))],
    );
    let mv = single(move_groups(&report));
    // difflib keeps the larger block in place, so it is `other` that moved.
    assert_eq!(mv.label, "moved other: within a.py");
    assert_eq!(report.stats().residual_units, 0);
}

#[test]
fn one_function_out_of_a_deleted_file() {
    let second = "def second(q):
    \"\"\"Another one.\"\"\"
    for item in q:
        yield item * 2
";
    let report = report_for(
        &[
            ("util.py", &format!("{HELPER}\n\n{second}")),
            ("core.py", "Y = 2\n"),
        ],
        &[(
            "core.py",
            &format!("Y = 2\n\n\n{HELPER}\n\ndef brand_new():\n    pass\n"),
        )],
    );
    let mv = single(move_groups(&report));
    assert_eq!(mv.label, "moved helper: util.py → core.py");
    let first = |u: &Unit| -> String {
        let lines = if u.old.is_empty() { &u.new } else { &u.old };
        lines
            .iter()
            .map(|ln| ln.text.clone())
            .find(|t| !t.trim().is_empty())
            .expect("a non-blank line")
    };
    let mut residual: Vec<(String, String)> = residual(&report)
        .into_iter()
        .map(|u| (u.path.clone(), first(u)))
        .collect();
    residual.sort();
    assert_eq!(
        residual,
        vec![
            ("core.py".to_string(), "def brand_new():".to_string()),
            ("util.py".to_string(), "def second(q):".to_string()),
        ]
    );
    // The deleted file's hunk still lists every line, now split across units.
    let hunk = single(
        report
            .hunks
            .values()
            .filter(|h| h.path == "util.py")
            .collect(),
    );
    assert!(hunk.lines.iter().all(|ln| {
        ln.unit
            .as_ref()
            .is_some_and(|u| report.units.contains_key(u))
    }));
    assert!(hunk.unit_ids.len() >= 2);
}

#[test]
fn function_moved_into_a_class() {
    let method = HELPER
        .replace("(x, y)", "(self, x, y)")
        .lines()
        .map(|ln| {
            if ln.is_empty() {
                ln.to_string()
            } else {
                format!("    {ln}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let cls = "class C:\n    def m(self):\n        return 1\n";
    let report = report_for(
        &[("a.py", &format!("{HELPER}\n\n{cls}"))],
        &[("a.py", &format!("{cls}\n{method}\n"))],
    );
    let _mv = single(move_groups(&report));
    let labels: HashSet<(SignatureKind, String)> = report
        .groups
        .iter()
        .filter(|g| !g.mechanical)
        .map(|g| (g.kind, g.label.clone()))
        .collect();
    assert_eq!(
        labels,
        HashSet::from([(
            SignatureKind::Args,
            "def helper(…) → def helper(…, …)".to_string()
        )])
    );
    let all_labels = report
        .groups
        .iter()
        .map(|g| g.label.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(!all_labels.contains("(indentation)"));
}

#[test]
fn import_path_change_is_explained_by_the_move() {
    let report = report_for(
        &[
            ("pkg/__init__.py", ""),
            ("pkg/a.py", HELPER),
            ("pkg/b.py", "X = 1\n"),
            (
                "pkg/c.py",
                "from pkg.a import helper\n\nprint(helper(1, 2))\n",
            ),
        ],
        &[
            ("pkg/__init__.py", ""),
            ("pkg/a.py", ""),
            ("pkg/b.py", &format!("X = 1\n\n\n{HELPER}")),
            (
                "pkg/c.py",
                "from pkg.b import helper\n\nprint(helper(1, 2))\n",
            ),
        ],
    );
    let mv = single(move_groups(&report));
    assert_eq!(mv.unit_ids.len(), 3);
    assert_eq!(report.stats().residual_units, 0);
    let imp = single(
        report
            .units
            .values()
            .filter(|u| u.path == "pkg/c.py")
            .collect(),
    );
    assert_eq!(
        imp.signature_kinds().collect::<Vec<_>>(),
        vec![SignatureKind::Move]
    );
}

#[test]
fn import_needed_by_the_moved_block_is_linked() {
    let block = "def stamp():\n    now = time.time()\n    label = str(now)\n    return label\n";
    let report = report_for(
        &[
            ("a.py", &format!("import time\n\n\n{block}")),
            ("b.py", "X = 1\n"),
        ],
        &[
            ("a.py", ""),
            ("b.py", &format!("import time\n\nX = 1\n\n\n{block}")),
        ],
    );
    let mv = single(move_groups(&report));
    assert_eq!(mv.unit_ids.len(), 4); // block out, block in, import out, import in
    assert_eq!(report.stats().residual_units, 0);
}

#[test]
fn relative_import_links_to_a_move_inside_the_package() {
    let report = report_for(
        &[
            ("pkg/__init__.py", ""),
            ("pkg/a.py", HELPER),
            ("pkg/b.py", "X = 1\n"),
            ("pkg/c.py", "from .a import helper\n\nprint(helper(1, 2))\n"),
        ],
        &[
            ("pkg/__init__.py", ""),
            ("pkg/a.py", ""),
            ("pkg/b.py", &format!("X = 1\n\n\n{HELPER}")),
            ("pkg/c.py", "from .b import helper\n\nprint(helper(1, 2))\n"),
        ],
    );
    let mv = single(move_groups(&report));
    assert_eq!(mv.unit_ids.len(), 3);
    assert_eq!(report.stats().residual_units, 0);
}

#[test]
fn repeated_import_path_change_groups() {
    let names = ["m0.py", "m1.py", "m2.py"];
    let before: Vec<(&str, &str)> = names
        .iter()
        .map(|n| (*n, "from a import x\n\nprint(x)\n"))
        .collect();
    let after: Vec<(&str, &str)> = names
        .iter()
        .map(|n| (*n, "from b import x\n\nprint(x)\n"))
        .collect();
    let report = report_for(&before, &after);
    let g = single(
        report
            .groups
            .iter()
            .filter(|g| g.kind == SignatureKind::Import)
            .collect(),
    );
    assert!(g.mechanical);
    assert_eq!(g.label, "a → b");
    assert_eq!(details(g), BTreeMap::from([("x", 3)]));
}

#[test]
fn added_parameter_groups_def_with_call_sites() {
    let before = "def fetch(a, b):\n    return a + b\n\n\nx = fetch(1, 2)\ny = fetch(3, 4)\n";
    let after = concat!(
        "def fetch(a, b, timeout=None):\n    return a + b\n\n\n",
        "x = fetch(1, 2, timeout=5)\ny = fetch(3, 4, timeout=cfg.t)\n"
    );
    let report = report_for(&[("a.py", before)], &[("a.py", after)]);
    let g = single(
        report
            .groups
            .iter()
            .filter(|g| g.kind == SignatureKind::Args)
            .collect(),
    );
    assert!(g.mechanical);
    assert_eq!(details(g), BTreeMap::from([("call", 2), ("definition", 1)]));
    assert_eq!(g.label, "def fetch(…) → def fetch(…, timeout=…)");
    assert_eq!(report.stats().residual_units, 0);
}

#[test]
fn moves_in_typescript_do_not_need_verification() {
    let func = concat!(
        "export function helper(x: number, y: number): number {\n  const total = x + y;\n",
        "  if (total > 10) {\n    return total * 2;\n  }\n  return total;\n}\n"
    );
    let report = report_for(
        &[
            ("a.ts", &format!("{func}\nexport const X = 1;\n")),
            ("b.ts", "export const Y = 2;\n"),
        ],
        &[
            ("a.ts", "export const X = 1;\n"),
            ("b.ts", &format!("export const Y = 2;\n\n{func}")),
        ],
    );
    let mv = single(move_groups(&report));
    assert_eq!(mv.label, "moved helper: a.ts → b.ts");
    assert_eq!(report.stats().residual_units, 0);
    assert!(!report.units.values().any(|u| u.verified));
}
