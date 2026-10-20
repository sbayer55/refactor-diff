//! In-classifier patterns on the real analyzers: list element changes (with the formatter's
//! reflow) and early-return guards.

mod common;

use common::*;
use refactor_diff_core::classify::{Classification, classify};
use refactor_diff_core::{LanguageAnalyzer, SignatureKind, UnitTag, analyzer_for};

fn ts() -> &'static dyn LanguageAnalyzer {
    analyzer_for("a.ts").expect("typescript analyzer")
}

fn py() -> &'static dyn LanguageAnalyzer {
    analyzer_for("x.py").expect("python analyzer")
}

/// Classify lines `old_range` of `old` against `new_range` of `new`.
fn ranged(
    lang: &dyn LanguageAnalyzer,
    old: &str,
    new: &str,
    old_range: Option<(u32, u32)>,
    new_range: Option<(u32, u32)>,
) -> Classification {
    let (a, b) = (lang.analyze(old), lang.analyze(new));
    let anchor = |r: Option<(u32, u32)>| r.map_or(0, |(first, _)| first - 1);
    classify(
        lang,
        &a,
        &b,
        old_range,
        new_range,
        anchor(old_range),
        anchor(new_range),
    )
}

fn kinds(cls: &Classification) -> Vec<SignatureKind> {
    cls.signatures.iter().map(|s| s.kind).collect()
}

#[test]
fn deps_array_reflow_after_removal() {
    let old = "useEffect(() => {\n  track();\n}, [\n  activeModule,\n  currentProgram?.name,\n  currentSite?.name,\n  institution?.name,\n  signedIn,\n  tenantId,\n]);\n";
    let new = "useEffect(() => {\n  track();\n}, [activeModule, currentSite?.name, institution?.name, signedIn, tenantId]);\n";
    let cls = ranged(ts(), old, new, Some((3, 10)), Some((3, 3)));
    assert_eq!(kinds(&cls), vec![SignatureKind::List]);
    let sig = &cls.signatures[0];
    assert_eq!(sig.key, "list\0array\0-currentProgram ?. name");
    assert_eq!(sig.label(), "delete currentProgram?.name");
    assert_eq!(sig.detail, "array");
    assert_eq!(cls.generic_tokens, 0);
}

#[test]
fn call_reflow_after_removing_an_object_member() {
    let old = "localStorage.setItem(\n  \"vertex:org-context\",\n  JSON.stringify({ siteId: mockSites[2]!.id, programId: null }),\n);\n";
    let new = "localStorage.setItem(\"vertex:org-context\", JSON.stringify({ siteId: mockSites[2]!.id }));\n";
    let cls = ranged(ts(), old, new, Some((1, 4)), Some((1, 1)));
    assert_eq!(kinds(&cls), vec![SignatureKind::List]);
    assert_eq!(cls.signatures[0].key, "list\0object\0-programId : null");
    assert_eq!(cls.signatures[0].label(), "delete programId: null");
}

#[test]
fn const_array_reflow_and_union_member_removal() {
    let old = "export const WORKSPACE_SLUGS = [\n  \"operations\",\n  \"teaching\",\n  \"programs\",\n  \"marketplace\",\n] as const;\n";
    let new = "export const WORKSPACE_SLUGS = [\"operations\", \"teaching\", \"marketplace\"] as const;\n";
    let cls = ranged(ts(), old, new, Some((1, 6)), Some((1, 1)));
    assert_eq!(cls.signatures[0].kind, SignatureKind::List);
    assert_eq!(cls.signatures[0].old, "\"programs\"");

    let old = "type Scope = \"site\" | \"program\" | \"tenant\";\n";
    let new = "type Scope = \"site\" | \"tenant\";\n";
    let cls = ranged(ts(), old, new, Some((1, 1)), Some((1, 1)));
    assert_eq!(kinds(&cls), vec![SignatureKind::List]);
    assert_eq!(cls.signatures[0].detail, "union");
    assert_eq!(cls.signatures[0].label(), "delete \"program\"");

    // A union member on its own line.
    let old = "type Scope =\n  | \"site\"\n  | \"program\"\n  | \"tenant\";\nconst x = 1;\n";
    let new = "type Scope =\n  | \"site\"\n  | \"tenant\";\nconst x = 1;\n";
    let cls = ranged(ts(), old, new, Some((3, 3)), None);
    assert_eq!(cls.signatures[0].kind, SignatureKind::List);
    assert_eq!(cls.signatures[0].detail, "union");
}

#[test]
fn reflow_with_only_a_trailing_comma_is_formatting() {
    let old = "import {\n  buildParticipantOptions,\n  isValidEmail,\n} from \"./participants\";\n";
    let new = "import { buildParticipantOptions, isValidEmail } from \"./participants\";\n";
    let cls = ranged(ts(), old, new, Some((1, 4)), Some((1, 1)));
    assert_eq!(kinds(&cls), vec![SignatureKind::Formatting]);
}

#[test]
fn mixed_removal_and_addition_falls_through() {
    let old = "f([\n  activeModule,\n  currentProgram?.name,\n  signedIn,\n]);\n";
    let new = "f([activeModule, institution?.name, signedIn]);\n";
    let cls = ranged(ts(), old, new, Some((1, 5)), Some((1, 1)));
    assert!(!kinds(&cls).contains(&SignatureKind::List));
    // The dropped trailing comma is not a signature of its own.
    assert!(cls.signatures.iter().all(|s| s.key != "replace\0,\0\x01"));
}

#[test]
fn deleted_member_lines_group_across_files() {
    let a_old = "export const a = {\n  id: \"s1\",\n  programId: \"p1\",\n  name: \"A\",\n};\n";
    let a_new = "export const a = {\n  id: \"s1\",\n  name: \"A\",\n};\n";
    let b_old = "const rows = [\n  {\n    id: 2,\n    programId: \"prog-2\",\n  },\n];\n";
    let b_new = "const rows = [\n  {\n    id: 2,\n  },\n];\n";
    let report = analyze_files(
        &[("src/a.ts", a_old), ("src/b.ts", b_old)],
        &[("src/a.ts", a_new), ("src/b.ts", b_new)],
    );
    let lists: Vec<_> = report
        .groups
        .iter()
        .filter(|g| g.kind == SignatureKind::List)
        .collect();
    assert_eq!(lists.len(), 1, "{:#?}", report.groups);
    let group = lists[0];
    assert_eq!(group.key, "list\0object\0-programId : <str>");
    assert_eq!(group.label, "delete programId: <str>");
    assert_eq!(group.unit_ids.len(), 2);
    assert!(group.mechanical);
}

#[test]
fn deleted_guard_is_tagged_precondition_removed() {
    let old = "function run() {\n  const id = currentProgram?.id;\n  if (!currentProgram?.id) {\n    toaster.error({ title: t(\"x\") });\n    return;\n  }\n  go(id);\n}\n";
    let new = "function run() {\n  const id = currentProgram?.id;\n  go(id);\n}\n";
    let report = analyze_files(&[("src/a.ts", old)], &[("src/a.ts", new)]);
    let unit = report
        .units
        .values()
        .find(|u| u.signature_kinds().any(|k| k == SignatureKind::Guard))
        .expect("a guard unit");
    assert!(unit.has_tag(UnitTag::PreconditionRemoved));
    assert!(!unit.explained, "guards are never mechanical");
    let sig = &unit.signatures[0];
    assert_eq!(sig.key, "guard\0-! currentProgram ?. id");
    assert_eq!(sig.label(), "delete if (!currentProgram?.id)");
}

#[test]
fn guard_with_else_is_not_a_guard() {
    let old = "function run() {\n  if (!ok) {\n    return;\n  } else {\n    go();\n  }\n}\n";
    let new = "function run() {\n}\n";
    let cls = ranged(ts(), old, new, Some((2, 6)), None);
    assert!(!kinds(&cls).contains(&SignatureKind::Guard));
    // Only the `if` part of an if/else.
    let new = "function run() {\n  {\n    go();\n  }\n}\n";
    let cls = ranged(ts(), old, new, Some((2, 4)), None);
    assert!(!kinds(&cls).contains(&SignatureKind::Guard));
}

#[test]
fn python_guards() {
    let old = "def f(x):\n    if not x: return\n    go(x)\n";
    let new = "def f(x):\n    go(x)\n";
    let cls = ranged(py(), old, new, Some((2, 2)), None);
    assert_eq!(kinds(&cls), vec![SignatureKind::Guard]);
    assert_eq!(cls.signatures[0].key, "guard\0-not x");
    assert_eq!(cls.signatures[0].old, "if not x");
    assert_eq!(cls.tags, vec![UnitTag::PreconditionRemoved]);

    let old = "def f(x):\n    if x is None:\n        raise ValueError(\"x\")\n    go(x)\n";
    let cls = ranged(py(), old, new, Some((2, 3)), None);
    assert_eq!(kinds(&cls), vec![SignatureKind::Guard]);
    assert_eq!(cls.signatures[0].key, "guard\0-x is None");

    // Added guard: not tagged.
    let cls = ranged(py(), new, old, None, Some((2, 3)));
    assert_eq!(cls.signatures[0].key, "guard\0+x is None");
    assert!(cls.tags.is_empty());

    // An if without an early exit is not a guard.
    let old = "def f(x):\n    if x:\n        go(x)\n    done()\n";
    let new = "def f(x):\n    done()\n";
    let cls = ranged(py(), old, new, Some((2, 3)), None);
    assert!(!kinds(&cls).contains(&SignatureKind::Guard));
}
