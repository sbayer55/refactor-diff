//! The JSON analyzer and the JSON-aware patterns (`json_links.rs`): fixture fields dropped
//! across records and i18n keys deleted together with their `t("key")` calls.

mod common;

use common::*;
use pretty_assertions::assert_eq;
use refactor_diff_core::json::members;
use refactor_diff_core::{
    FileAnalysis, Report, SignatureKind, StmtSpan, TokenKind, Unit, UnitTag, analyzer_for,
};

fn analyze_json(text: &str) -> FileAnalysis {
    analyzer_for("data.json")
        .expect("json analyzer")
        .analyze(text)
}

fn units_in<'r>(report: &'r Report, path: &str) -> Vec<&'r Unit> {
    report.units.values().filter(|u| u.path == path).collect()
}

fn spans(statements: &[StmtSpan]) -> Vec<(u32, u32, usize)> {
    statements
        .iter()
        .map(|s| (s.start, s.end, s.children.len()))
        .collect()
}

#[test]
fn json_analyzer_dispatch() {
    assert_eq!(analyzer_for("a/b.json").map(|a| a.name()), Some("json"));
    assert!(analyzer_for("package-lock.json").is_none());
    assert!(analyzer_for("web/package-lock.json").is_none());
    assert!(analyzer_for("deps.lock.json").is_none());
    assert!(analyzer_for("a.jsonc").is_none());
}

#[test]
fn json_tokens() {
    let an = analyze_json("{\n  \"a\": \"x\",\n  \"b\": [1, true, null]\n}\n");
    assert!(an.parsed);
    let toks: Vec<(TokenKind, &str)> = an
        .tokens
        .iter()
        .map(|t| (t.kind, t.value.as_str()))
        .collect();
    use TokenKind::*;
    assert_eq!(
        toks,
        vec![
            (Op, "{"),
            (String, "'a'"),
            (Op, ":"),
            (String, "'x'"),
            (Structural, "<NEWLINE>"),
            (String, "'b'"),
            (Op, ":"),
            (Op, "["),
            (Number, "1"),
            (Structural, "<NEWLINE>"),
            (Name, "true"),
            (Structural, "<NEWLINE>"),
            (Name, "null"),
            (Structural, "<NEWLINE>"),
            (Op, "]"),
            (Structural, "<NEWLINE>"),
            (Op, "}"),
            (Structural, "<NEWLINE>"),
        ]
    );
    // Separating commas fold into the member's line end, so layout changes are formatting.
    let a = analyze_json("{\"a\": 1, \"b\": 2}");
    let b = analyze_json("{\n  \"a\": 1,\n  \"b\": 2\n}");
    let vals = |an: &FileAnalysis| {
        an.tokens
            .iter()
            .map(|t| t.value.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(vals(&a), vals(&b));
    // Escapes compare by value.
    let c = analyze_json("{\"a\": \"\\u0041\"}");
    assert_eq!(c.tokens[3].value, "'A'");
    assert_eq!(c.tokens[3].text, "\"\\u0041\"");
}

#[test]
fn json_statements_and_paths() {
    let text = "[\n  {\n    \"id\": 1,\n    \"tags\": {\"x\": 1}\n  },\n  {\"id\": 2}\n]\n";
    let an = analyze_json(text);
    assert!(an.imports.is_empty() && an.calls.is_empty() && an.defs.is_empty());
    assert_eq!(spans(&an.statements), vec![(2, 5, 2), (6, 6, 1)]);
    assert_eq!(
        spans(&an.statements[0].children),
        vec![(3, 3, 0), (4, 4, 1)]
    );
    let syntax = an.syntax.as_ref().expect("kept for json_links");
    let paths: Vec<(String, String, Option<String>)> = members(syntax, &an.lines)
        .into_iter()
        .map(|m| (m.path, m.norm, m.key))
        .collect();
    let s = |v: &str| v.to_string();
    assert_eq!(
        paths,
        vec![
            (s(""), s(""), None),
            (s("[0]"), s("[]"), None),
            (s("[0].id"), s("[].id"), Some(s("id"))),
            (s("[0].tags"), s("[].tags"), Some(s("tags"))),
            (s("[0].tags.x"), s("[].tags.x"), Some(s("x"))),
            (s("[1]"), s("[]"), None),
            (s("[1].id"), s("[].id"), Some(s("id"))),
        ]
    );
}

#[test]
fn oversized_json_is_tokenized_by_line() {
    let body: String = (0..120_000)
        .map(|i| format!("  \"key{i}\": \"value {i}\",\n"))
        .collect();
    let text = format!("{{\n{body}  \"last\": 0\n}}\n");
    assert!(text.len() > refactor_diff_core::json::MAX_PARSE_BYTES);
    let an = analyze_json(&text);
    assert!(!an.parsed);
    assert_eq!(an.tokens.len(), an.lines.len());
    assert_eq!(an.tokens[1].value, "\"key0\": \"value 0\",");
}

const EVENTS_BEFORE: &str = r#"[
  {
    "id": "e1",
    "title": "A",
    "programId": "prog-001",
    "room": "r1"
  },
  {
    "id": "e2",
    "title": "B",
    "programId": "prog-002",
    "room": "r2"
  }
]
"#;

const EVENTS_AFTER: &str = r#"[
  {
    "id": "e1",
    "title": "A",
    "room": "r1"
  },
  {
    "id": "e2",
    "title": "B",
    "room": "r2"
  }
]
"#;

const STAFF_BEFORE: &str = r#"[
  {
    "name": "Ann",
    "version": 1,
    "programId": "prog-nursing"
  }
]
"#;

const STAFF_AFTER: &str = r#"[
  {
    "name": "Ann",
    "version": 1
  }
]
"#;

#[test]
fn fixture_field_dropped_across_files_groups() {
    let report = analyze_files(
        &[
            ("mocks/events.json", EVENTS_BEFORE),
            ("mocks/staff.json", STAFF_BEFORE),
        ],
        &[
            ("mocks/events.json", EVENTS_AFTER),
            ("mocks/staff.json", STAFF_AFTER),
        ],
    );
    assert!(report.files.iter().all(|f| f.analyzed && f.parse_ok));
    let g = report
        .groups
        .iter()
        .find(|g| g.kind == SignatureKind::Fixture)
        .expect("a fixture group");
    assert_eq!(g.key, "fixture\0[].programId");
    assert_eq!(g.label, "delete programId");
    assert!(g.mechanical);
    assert_eq!(g.unit_ids.len(), 3);
    assert_eq!(g.files, vec!["mocks/events.json", "mocks/staff.json"]);
    // The last member's comma drop on the line above folds into the deletion.
    assert!(report.units.values().all(|u| u.explained));
    assert_eq!(report.stats().residual_units, 0);
}

#[test]
fn deleted_record_is_not_a_fixture_field() {
    let before = "[\n  {\"id\": 1},\n  {\n    \"id\": 2,\n    \"name\": \"x\"\n  }\n]\n";
    let after = "[\n  {\"id\": 1}\n]\n";
    let report = analyze_files(&[("mocks/a.json", before)], &[("mocks/a.json", after)]);
    assert!(
        report
            .groups
            .iter()
            .all(|g| g.kind != SignatureKind::Fixture)
    );
}

const LOCALE_BEFORE: &str = r#"{
  "setup": {
    "description": "Add one site and one program.",
    "title": "Setup",
    "stepPrograms": "Programs",
    "toast": {
      "outOfProgram": "{{count}} out of program"
    }
  }
}
"#;

const LOCALE_AFTER: &str = r#"{
  "setup": {
    "description": "Add one site.",
    "title": "Setup",
    "toast": {
      "outOfSite": "{{count}} outside the site"
    }
  }
}
"#;

const STEPS_BEFORE: &str = r#"export function Steps() {
  const { t } = useTranslation();
  return (
    <ol>
      <li>{t("setup.title")}</li>
      <li>{t("setup.stepPrograms")}</li>
    </ol>
  );
}
"#;

const STEPS_AFTER: &str = r#"export function Steps() {
  const { t } = useTranslation();
  return (
    <ol>
      <li>{t("setup.title")}</li>
    </ol>
  );
}
"#;

fn locale_report() -> Report {
    analyze_files(
        &[
            ("src/i18n/locales/en/common.json", LOCALE_BEFORE),
            ("src/Steps.tsx", STEPS_BEFORE),
        ],
        &[
            ("src/i18n/locales/en/common.json", LOCALE_AFTER),
            ("src/Steps.tsx", STEPS_AFTER),
        ],
    )
}

#[test]
fn i18n_key_deletion_pairs_with_t_call_deletion() {
    let report = locale_report();
    let g = report
        .groups
        .iter()
        .find(|g| g.key == "i18n\0setup.stepPrograms")
        .expect("an i18n group for the deleted key");
    assert_eq!(g.kind, SignatureKind::I18n);
    assert_eq!(g.label, "delete setup.stepPrograms");
    assert!(g.mechanical);
    assert_eq!(
        g.files,
        vec!["src/Steps.tsx", "src/i18n/locales/en/common.json"]
    );
    let tsx = units_in(&report, "src/Steps.tsx");
    assert_eq!(tsx.len(), 1);
    assert!(tsx[0].explained);
}

#[test]
fn value_edit_and_key_rename_stay_residual() {
    let report = locale_report();
    let locale = units_in(&report, "src/i18n/locales/en/common.json");
    // `description` changed text: not an i18n deletion.
    let desc = locale
        .iter()
        .find(|u| u.old.iter().any(|l| l.text.contains("\"description\"")))
        .expect("the description edit");
    assert!(!desc.explained);
    assert!(
        desc.signatures
            .iter()
            .all(|s| s.kind != SignatureKind::I18n)
    );
    // `outOfProgram` -> `outOfSite` is a rename: kept as a replace, not swallowed.
    let renamed = locale
        .iter()
        .find(|u| u.old.iter().any(|l| l.text.contains("outOfProgram")))
        .expect("the key rename");
    assert!(!renamed.explained);
    assert!(
        renamed
            .signatures
            .iter()
            .all(|s| s.kind == SignatureKind::Replace)
    );
    assert!(
        report
            .groups
            .iter()
            .all(|g| g.kind != SignatureKind::I18n || !g.key.contains("outOfProgram"))
    );
}

#[test]
fn t_call_inside_other_deleted_code_is_not_paired() {
    let before = "export function A() {\n  return (\n    <div>\n      {t(\"setup.stepPrograms\")}\n      <ProgramPicker value={program} onChange={setProgram} disabled={!ready} />\n    </div>\n  );\n}\n";
    let after = "export function A() {\n  return (\n    <div>\n    </div>\n  );\n}\n";
    let report = analyze_files(
        &[
            ("src/i18n/locales/en/common.json", LOCALE_BEFORE),
            ("src/A.tsx", before),
        ],
        &[
            ("src/i18n/locales/en/common.json", LOCALE_AFTER),
            ("src/A.tsx", after),
        ],
    );
    let tsx = units_in(&report, "src/A.tsx");
    assert!(
        tsx.iter()
            .all(|u| u.signatures.iter().all(|s| s.kind != SignatureKind::I18n))
    );
    let g = report
        .groups
        .iter()
        .find(|g| g.key == "i18n\0setup.stepPrograms")
        .expect("the json side still carries the key");
    assert!(!g.mechanical);
}

#[test]
fn locale_unit_mixing_a_deletion_and_a_value_edit_keeps_its_replace() {
    let before = "{\n  \"a\": {\n    \"x\": \"1\",\n    \"desc\": \"old text\",\n    \"gone\": \"G\",\n    \"y\": \"2\"\n  }\n}\n";
    let after =
        "{\n  \"a\": {\n    \"x\": \"1\",\n    \"desc\": \"new text\",\n    \"y\": \"2\"\n  }\n}\n";
    let path = "src/locales/en.json";
    let report = analyze_files(&[(path, before)], &[(path, after)]);
    let units = units_in(&report, path);
    let kinds: Vec<SignatureKind> = units.iter().flat_map(|u| u.signature_kinds()).collect();
    assert!(kinds.contains(&SignatureKind::I18n));
    assert!(kinds.contains(&SignatureKind::Replace));
    assert!(units.iter().all(|u| !u.explained));
    assert!(report.groups.iter().any(|g| g.key == "i18n\0a.gone"));
}

#[test]
fn a_removed_guard_keeps_its_signature_when_its_message_keys_go() {
    let before = r#"export function save(id: string | null) {
  if (!id) {
    toast(t("setup.stepPrograms"));
    return;
  }
  send(id);
}
"#;
    let after = r#"export function save(id: string | null) {
  send(id);
}
"#;
    let report = analyze_files(
        &[
            ("src/i18n/locales/en/common.json", LOCALE_BEFORE),
            ("src/save.ts", before),
        ],
        &[
            ("src/i18n/locales/en/common.json", LOCALE_AFTER),
            ("src/save.ts", after),
        ],
    );
    let units = units_in(&report, "src/save.ts");
    assert_eq!(units.len(), 1);
    assert!(units[0].has_tag(UnitTag::PreconditionRemoved));
    assert!(!units[0].explained);
    assert!(
        units[0]
            .signatures
            .iter()
            .all(|s| s.kind == SignatureKind::Guard)
    );
}
