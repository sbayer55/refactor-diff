//! The questions every needs-review hunk is asked, and the smaller set each sampled
//! mechanical group is asked. Change the wording or the criteria and bump
//! [`QUESTION_SET_VERSION`] (or [`GROUP_QUESTION_SET_VERSION`]): cached answers are only
//! reused for the same version.

use serde_json::{Value, json};

use super::{Question, QuestionKind, Questions};

/// Part of the cache key; bump whenever the questions below change.
pub const QUESTION_SET_VERSION: &str = "2";
/// Part of the group cache key; bump whenever [`group_questions`] changes.
pub const GROUP_QUESTION_SET_VERSION: &str = "1";

/// The yes/no concerns that feed `concern_max`, in display order.
pub const BOOLEAN_CONCERNS: [&str; 8] = [
    "hardcoded_secret",
    "injection_risk",
    "touches_auth",
    "weakens_tests",
    "breaks_api",
    "data_migration",
    "debug_leftovers",
    "removes_guard_or_validation",
];

/// Asked as a boolean but kept out of [`BOOLEAN_CONCERNS`]: it nudges `risk` instead of
/// flagging the hunk.
pub const BEYOND_STATED_CHANGE: &str = "behavior_beyond_stated_change";

/// Ordered rungs of `blast_radius`; the text before the colon is the label.
pub const BLAST_RADIUS: [&str; 4] = [
    "none: comments, docs or formatting only",
    "local: one function or statement",
    "module: one file or module",
    "core: shared infrastructure, a public API or many callers",
];

/// Ordered rungs of `reviewer_effort`.
pub const REVIEWER_EFFORT: [&str; 3] = [
    "glance: obvious on sight",
    "careful read: the logic has to be followed",
    "expert: a domain expert or security review",
];

/// The options of `primary_concern`, with the description Jev sees.
pub const PRIMARY_CONCERN: [(&str, &str); 11] = [
    (
        "secret",
        "a credential, token or key is written into the code",
    ),
    (
        "injection",
        "untrusted input reaches a query, shell, path or template",
    ),
    (
        "auth",
        "authentication, authorization or session handling changes",
    ),
    (
        "api_contract",
        "a public signature, wire format or schema changes",
    ),
    (
        "test_weakening",
        "a test is deleted, skipped or its assertions loosened",
    ),
    (
        "debug",
        "debug prints, commented-out code, TODO hacks or leftover flags",
    ),
    ("migration", "stored data or its schema changes"),
    (
        "guard_removed",
        "an early-return guard, validation or permission check is removed",
    ),
    ("logic", "behavior changes that fit none of the above"),
    ("none", "no notable risk: a mechanical or cosmetic change"),
    (
        "insufficient_evidence",
        "the hunk alone does not show enough to judge",
    ),
];

/// The options of `change_role`: how the hunk relates to the change the PR is about.
pub const CHANGE_ROLE: [(&str, &str); 5] = [
    ("core", "the change the PR is about"),
    (
        "cascade",
        "follow-on edits forced by the core change: call sites, types, props",
    ),
    (
        "cleanup",
        "dead code, imports or comments left behind by the core change",
    ),
    ("test-fixture", "tests, mocks, fixtures or stories"),
    ("collateral", "anything else that changes behaviour"),
];

const PREAMBLE: &str = "The state describes one hunk of a code review diff. `hunk` is the \
numbered unified diff (`-` removed, `+` added); `enclosing` is the surrounding function \
before and after when known; `adds_tests` and `docs_only` are facts the tool computed from \
the file; `patterns` lists repeated mechanical edits already collapsed elsewhere in the diff; \
`stated_change`, when present, is what the change says it does (the PR title, or the concept \
the diff removes); `pr_body`, when present, is the start of the pull request description.";

const GROUP_PREAMBLE: &str = "The state describes one repeated edit that a tool collapsed in \
a code review diff as mechanical. `pattern` is the edit (`kind`, `label`, `old` → `new`); \
`occurrences` are numbered unified diffs (`-` removed, `+` added) of a few sampled places \
where it was applied; `stated_change`, when present, is what the change says it does.";

fn boolean(instructions: &str, yes: &str, no: &str) -> Question {
    Question {
        kind: QuestionKind::Boolean,
        instructions: format!("{PREAMBLE} {instructions}"),
        criteria: Some(json!({"true": yes, "false": no})),
    }
}

fn choice(instructions: &str, options: &[(&str, &str)]) -> Question {
    let criteria: serde_json::Map<String, Value> = options
        .iter()
        .map(|(name, desc)| (name.to_string(), Value::String(desc.to_string())))
        .collect();
    Question {
        kind: QuestionKind::Choice,
        instructions: format!("{PREAMBLE} {instructions}"),
        criteria: Some(Value::Object(criteria)),
    }
}

fn score(instructions: &str, rungs: &[&str]) -> Question {
    Question {
        kind: QuestionKind::Score,
        instructions: format!("{PREAMBLE} {instructions}"),
        criteria: Some(Value::Array(
            rungs.iter().map(|r| Value::String(r.to_string())).collect(),
        )),
    }
}

/// The full question set, in the order it is sent.
pub fn questions() -> Questions {
    let mut q = Questions::new();
    q.insert(
        "hardcoded_secret".into(),
        boolean(
            "Does the added code hard-code a secret?",
            "a password, API key, token, private key or connection string with credentials appears literally in added lines",
            "no literal credential is added; placeholders, environment lookups and test fixtures do not count",
        ),
    );
    q.insert(
        "injection_risk".into(),
        boolean(
            "Does the change let untrusted input reach an interpreter unescaped?",
            "user or external input is concatenated or formatted into SQL, a shell command, a file path, HTML or a template without parameterization or escaping",
            "inputs are parameterized, escaped, validated, or no external input is involved",
        ),
    );
    q.insert(
        "touches_auth".into(),
        boolean(
            "Does the change touch authentication, authorization or session handling?",
            "login, tokens, permissions, roles, access checks, password handling or session state are added, removed or altered",
            "none of that is involved",
        ),
    );
    q.insert(
        "weakens_tests".into(),
        boolean(
            "Does the change weaken test coverage?",
            "a test or assertion is removed, skipped, marked expected-failure, or its expected values loosened",
            "tests are unchanged, added or strengthened, or the file is not a test",
        ),
    );
    q.insert(
        "breaks_api".into(),
        boolean(
            "Does the change alter a public contract?",
            "an exported function signature, HTTP route, wire format, schema, CLI flag or config key changes or disappears in a way callers would notice",
            "only private or internal code changes, or the contract is extended compatibly",
        ),
    );
    q.insert(
        "data_migration".into(),
        boolean(
            "Does the change alter stored data or its schema?",
            "a database migration, a storage format, a serialized file layout or persisted state changes",
            "nothing persisted is affected",
        ),
    );
    q.insert(
        "debug_leftovers".into(),
        boolean(
            "Does the added code leave debugging artifacts behind?",
            "print or console statements, commented-out code, hard-coded local paths, disabled checks or temporary flags remain in added lines",
            "the added code is clean",
        ),
    );
    q.insert(
        "removes_guard_or_validation".into(),
        boolean(
            "Does the change remove a guard, validation or permission check?",
            "an early return or throw guard, an input validation, a null or bounds check, or a permission or feature check is deleted or bypassed",
            "no check is removed, or one is only moved or replaced by an equivalent",
        ),
    );
    q.insert(
        BEYOND_STATED_CHANGE.into(),
        boolean(
            "Does this hunk change behaviour beyond the stated change (`stated_change` in the state)? Mechanical consequences of the stated change (removing a field that the change removes, deleting code that used it) are not beyond it.",
            "the hunk alters behaviour that the stated change does not account for",
            "the hunk is the stated change or a mechanical consequence of it, or no change is stated and the hunk is behaviour-neutral",
        ),
    );
    q.insert(
        "blast_radius".into(),
        score(
            "How much of the program can this change affect?",
            &BLAST_RADIUS,
        ),
    );
    q.insert(
        "reviewer_effort".into(),
        score(
            "How much attention does a reviewer need to give this hunk?",
            &REVIEWER_EFFORT,
        ),
    );
    q.insert(
        "primary_concern".into(),
        choice(
            "What is the single most important thing a reviewer should check in this hunk?",
            &PRIMARY_CONCERN,
        ),
    );
    q.insert(
        "change_role".into(),
        choice(
            "What role does this hunk play relative to the stated change (`stated_change` in the state, else the change as a whole)?",
            &CHANGE_ROLE,
        ),
    );
    q
}

/// The questions asked once per sampled mechanical group.
pub fn group_questions() -> Questions {
    let mut q = Questions::new();
    q.insert(
        "same_edit".into(),
        Question {
            kind: QuestionKind::Boolean,
            instructions: format!(
                "{GROUP_PREAMBLE} Are all sampled occurrences the same mechanical edit, with no occurrence changing behaviour differently?"
            ),
            criteria: Some(json!({
                "true": "every occurrence applies the same edit and none of them does anything else",
                "false": "at least one occurrence differs in kind, or changes behaviour in a way the others do not",
            })),
        },
    );
    q
}

/// The label of a score rung: the text before the colon.
pub fn rung_label(rungs: &[&str], index: usize) -> String {
    rungs
        .get(index)
        .map(|r| r.split(':').next().unwrap_or(r).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn question_set_shape() {
        let q = questions();
        assert_eq!(q.len(), 13);
        let names: Vec<&str> = q.keys().map(String::as_str).collect();
        assert_eq!(&names[..8], &BOOLEAN_CONCERNS[..]);
        assert_eq!(
            &names[8..],
            &[
                BEYOND_STATED_CHANGE,
                "blast_radius",
                "reviewer_effort",
                "primary_concern",
                "change_role"
            ]
        );
        assert_eq!(q[BEYOND_STATED_CHANGE].kind, QuestionKind::Boolean);
        assert!(!BOOLEAN_CONCERNS.contains(&BEYOND_STATED_CHANGE));
        for name in BOOLEAN_CONCERNS {
            assert_eq!(q[name].kind, QuestionKind::Boolean);
            assert!(q[name].criteria.as_ref().unwrap()["true"].is_string());
        }
        let v = serde_json::to_value(&q).unwrap();
        assert_eq!(v["blast_radius"]["criteria"].as_array().unwrap().len(), 4);
        assert_eq!(
            v["reviewer_effort"]["criteria"].as_array().unwrap().len(),
            3
        );
        assert_eq!(
            v["primary_concern"]["criteria"].as_object().unwrap().len(),
            11
        );
        assert!(v["primary_concern"]["criteria"]["insufficient_evidence"].is_string());
        assert!(v["primary_concern"]["criteria"]["guard_removed"].is_string());
        let roles = v["change_role"]["criteria"].as_object().unwrap();
        assert_eq!(roles.len(), 5);
        for (name, _) in CHANGE_ROLE {
            assert!(roles[name].is_string(), "{name}");
        }
        assert!(
            v[BEYOND_STATED_CHANGE]["instructions"]
                .as_str()
                .unwrap()
                .contains("`stated_change`")
        );
        assert!(
            v["touches_auth"]["instructions"]
                .as_str()
                .unwrap()
                .contains("`hunk`")
        );
    }

    #[test]
    fn group_question_set_shape() {
        let q = group_questions();
        assert_eq!(q.len(), 1);
        assert_eq!(q["same_edit"].kind, QuestionKind::Boolean);
        let text = &q["same_edit"].instructions;
        assert!(text.contains("`occurrences`") && text.contains("`pattern`"));
    }

    #[test]
    fn rung_labels() {
        assert_eq!(rung_label(&BLAST_RADIUS, 0), "none");
        assert_eq!(rung_label(&BLAST_RADIUS, 3), "core");
        assert_eq!(rung_label(&REVIEWER_EFFORT, 1), "careful read");
        assert_eq!(rung_label(&REVIEWER_EFFORT, 9), "unknown");
    }
}
