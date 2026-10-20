//! Workstream B correctness fixes: partial multi-line imports, group labels from the most
//! common member, and substitutions told apart from renames.

mod common;

use std::collections::BTreeSet;

use common::*;
use indexmap::IndexMap;
use refactor_diff_core::{Line, Report, Signature, SignatureKind, Unit, build_groups};

/// Every signature key of every unit in `path`.
fn keys_in(report: &Report, path: &str) -> BTreeSet<String> {
    report
        .units
        .values()
        .filter(|u| u.path == path)
        .flat_map(|u| u.signatures.iter().map(|s| s.key.clone()))
        .collect()
}

fn set(keys: &[&str]) -> BTreeSet<String> {
    keys.iter().map(|k| k.to_string()).collect()
}

const TS_BEFORE: &str = concat!(
    "import {\n",
    "  getSites,\n",
    "  createProgram,\n",
    "  updateSite,\n",
    "  deleteProgram,\n",
    "} from \"@/services/organization\";\n",
    "\n",
    "export const sites = getSites;\n",
    "export const update = updateSite;\n",
);

const TS_AFTER: &str = concat!(
    "import {\n",
    "  getSites,\n",
    "  updateSite,\n",
    "} from \"@/services/organization\";\n",
    "\n",
    "export const sites = getSites;\n",
    "export const update = updateSite;\n",
);

#[test]
fn ts_dropping_two_members_signs_only_those_members() {
    let report = analyze_files(&[("q.ts", TS_BEFORE)], &[("q.ts", TS_AFTER)]);
    assert_eq!(
        keys_in(&report, "q.ts"),
        set(&[
            "import\0@/services/organization.createProgram\0",
            "import\0@/services/organization.deleteProgram\0",
        ])
    );
    for g in &report.groups {
        assert!(!g.label.contains("getSites"), "{}", g.label);
        assert!(!g.label.contains("updateSite"), "{}", g.label);
    }
}

#[test]
fn ts_adding_two_members_signs_only_those_members() {
    let report = analyze_files(&[("q.ts", TS_AFTER)], &[("q.ts", TS_BEFORE)]);
    assert_eq!(
        keys_in(&report, "q.ts"),
        set(&[
            "import\0\0@/services/organization.createProgram",
            "import\0\0@/services/organization.deleteProgram",
        ])
    );
}

#[test]
fn ts_adjacent_members_dropped_in_one_unit() {
    let before = "import {\n  a,\n  b,\n  c,\n  d,\n} from \"m\";\n\nexport const x = a + d;\n";
    let after = "import {\n  a,\n  d,\n} from \"m\";\n\nexport const x = a + d;\n";
    let report = analyze_files(&[("q.ts", before)], &[("q.ts", after)]);
    assert_eq!(
        keys_in(&report, "q.ts"),
        set(&["import\0m.b\0", "import\0m.c\0"])
    );
}

const PY_BEFORE: &str = concat!(
    "from services.organization import (\n",
    "    get_sites,\n",
    "    create_program,\n",
    "    update_site,\n",
    "    delete_program,\n",
    ")\n",
    "\n",
    "SITES = get_sites\n",
    "UPDATE = update_site\n",
);

const PY_AFTER: &str = concat!(
    "from services.organization import (\n",
    "    get_sites,\n",
    "    update_site,\n",
    ")\n",
    "\n",
    "SITES = get_sites\n",
    "UPDATE = update_site\n",
);

#[test]
fn python_dropping_two_members_signs_only_those_members() {
    let report = analyze_files(&[("q.py", PY_BEFORE)], &[("q.py", PY_AFTER)]);
    assert_eq!(
        keys_in(&report, "q.py"),
        set(&[
            "import\0services.organization.create_program\0",
            "import\0services.organization.delete_program\0",
        ])
    );
}

#[test]
fn python_adding_two_members_signs_only_those_members() {
    let report = analyze_files(&[("q.py", PY_AFTER)], &[("q.py", PY_BEFORE)]);
    assert_eq!(
        keys_in(&report, "q.py"),
        set(&[
            "import\0\0services.organization.create_program",
            "import\0\0services.organization.delete_program",
        ])
    );
}

#[test]
fn deleting_a_whole_import_statement_is_unchanged() {
    let before = "import { a } from \"m\";\nimport { b } from \"n\";\n\nexport const x = a;\n";
    let after = "import { a } from \"m\";\n\nexport const x = a;\n";
    let report = analyze_files(&[("q.ts", before)], &[("q.ts", after)]);
    assert_eq!(keys_in(&report, "q.ts"), set(&["import\0n.b\0"]));
}

// ---------------------------------------------------------------- group labels ---

fn unit(uid: &str, signatures: Vec<Signature>) -> Unit {
    Unit {
        id: uid.to_string(),
        path: format!("{uid}.py"),
        hunk_id: "h".into(),
        old_start: 1,
        new_start: 1,
        old: vec![],
        new: vec![Line {
            text: "x = 1".into(),
            hl: vec![],
        }],
        signatures,
        explained: false,
        partner: None,
        verified: false,
        near: vec![],
        tags: vec![],
    }
}

fn args(old: &str, new: &str) -> Signature {
    Signature::new(SignatureKind::Args, "args\0k", old, new)
}

#[test]
fn group_label_comes_from_the_most_common_member() {
    let mut units: IndexMap<String, Unit> = [
        unit("1", vec![args("def f(…)", "def f(…, t=…)")]),
        unit("2", vec![args("f(…)", "f(…, t=…)")]),
        unit("3", vec![args("f(…)", "f(…, t=…)")]),
        unit("4", vec![args("g.f(…)", "g.f(…, t=…)")]),
    ]
    .into_iter()
    .map(|u| (u.id.clone(), u))
    .collect();
    let groups = build_groups(&mut units, 2);
    let [g] = &groups[..] else {
        panic!("one group expected: {groups:?}");
    };
    assert_eq!(g.label, "f(…) → f(…, t=…) (+2 variants)");
    assert_eq!((g.old.as_str(), g.new.as_str()), ("f(…)", "f(…, t=…)"));
}

#[test]
fn group_label_ties_go_to_the_first_seen_and_uniform_groups_have_no_suffix() {
    let mut units: IndexMap<String, Unit> = [
        unit("1", vec![args("a(…)", "a(…, t=…)")]),
        unit("2", vec![args("b(…)", "b(…, t=…)")]),
    ]
    .into_iter()
    .map(|u| (u.id.clone(), u))
    .collect();
    let groups = build_groups(&mut units, 2);
    assert_eq!(groups[0].label, "a(…) → a(…, t=…) (+1 variants)");

    let mut units: IndexMap<String, Unit> = [
        unit("1", vec![args("a(…)", "a(…, t=…)")]),
        unit("2", vec![args("a(…)", "a(…, t=…)")]),
    ]
    .into_iter()
    .map(|u| (u.id.clone(), u))
    .collect();
    let groups = build_groups(&mut units, 2);
    assert_eq!(groups[0].label, "a(…) → a(…, t=…)");
}

// ---------------------------------------------------------------- substitution ---

fn group_kind(report: &Report, label: &str) -> Option<SignatureKind> {
    report
        .groups
        .iter()
        .find(|g| g.label == label)
        .map(|g| g.kind)
}

#[test]
fn rename_to_a_name_already_in_the_base_is_a_substitution() {
    let before = concat!(
        "export function f(siteId: string, programId: string) {\n",
        "  log(siteId);\n",
        "  a(programId);\n",
        "  b(programId);\n",
        "}\n",
    );
    let after = concat!(
        "export function f(siteId: string, programId: string) {\n",
        "  log(siteId);\n",
        "  a(siteId);\n",
        "  b(siteId);\n",
        "}\n",
    );
    let report = analyze_files(&[("r.ts", before)], &[("r.ts", after)]);
    let g = report
        .groups
        .iter()
        .find(|g| g.label == "programId → siteId")
        .expect("substitution group");
    assert_eq!(g.kind, SignatureKind::Substitution);
    assert_eq!(g.key, "substitution\0programId\0siteId");
    assert!(!g.mechanical, "substitution is never mechanical");
    assert!(report.units.values().all(|u| !u.explained));
}

#[test]
fn name_from_another_files_base_also_counts() {
    let report = analyze_files(
        &[
            ("a.ts", "export const siteId = 1;\n"),
            ("b.ts", "use(programId);\nuse(programId);\n"),
        ],
        &[
            ("a.ts", "export const siteId = 2;\n"),
            ("b.ts", "use(siteId);\nuse(siteId);\n"),
        ],
    );
    assert_eq!(
        group_kind(&report, "programId → siteId"),
        Some(SignatureKind::Substitution)
    );
}

#[test]
fn new_name_only_on_deleted_lines_stays_a_rename() {
    // The old `fetchUser` is deleted and `getUser` takes its name: a real rename.
    let before = concat!(
        "const fetchUser = 1;\n",
        "const keep = 0;\n",
        "getUser();\n",
        "getUser();\n",
    );
    let after = concat!("const keep = 0;\n", "fetchUser();\n", "fetchUser();\n");
    let report = analyze_files(&[("u.ts", before)], &[("u.ts", after)]);
    assert_eq!(
        group_kind(&report, "getUser → fetchUser"),
        Some(SignatureKind::Rename)
    );

    let before = "export function oldName() {}\noldName();\noldName();\n";
    let after = "export function newName() {}\nnewName();\nnewName();\n";
    let report = analyze_files(&[("v.ts", before)], &[("v.ts", after)]);
    let g = report
        .groups
        .iter()
        .find(|g| g.label == "oldName → newName")
        .unwrap();
    assert_eq!(g.kind, SignatureKind::Rename);
    assert!(g.mechanical);
}

#[test]
fn string_literal_swapped_for_an_existing_one_is_a_substitution() {
    let report = analyze_files(
        &[
            ("fixtures.ts", "export const SITE = \"site-001\";\n"),
            ("t.ts", "f(\"prog-001\");\ng(\"prog-001\");\n"),
        ],
        &[
            ("fixtures.ts", "export const SITE = \"site-001\";\n// x\n"),
            ("t.ts", "f(\"site-001\");\ng(\"site-001\");\n"),
        ],
    );
    let g = report
        .groups
        .iter()
        .find(|g| g.kind == SignatureKind::Substitution)
        .expect("substitution group");
    assert_eq!(g.label, "\"prog-001\" → \"site-001\"");
    assert!(!g.mechanical);

    // A literal new to the base stays a mechanical replace.
    let report = analyze_files(
        &[("t.ts", "f(\"a\");\ng(\"a\");\n")],
        &[("t.ts", "f(\"b\");\ng(\"b\");\n")],
    );
    let g = report
        .groups
        .iter()
        .find(|g| g.label == "\"a\" → \"b\"")
        .unwrap();
    assert_eq!(g.kind, SignatureKind::Replace);
    assert!(g.mechanical);
}
