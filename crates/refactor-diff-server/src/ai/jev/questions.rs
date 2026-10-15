//! The questions every needs-review hunk is asked. Change the wording or the criteria and
//! bump [`QUESTION_SET_VERSION`]: cached answers are only reused for the same version.

use serde_json::{Value, json};

use super::{Question, QuestionKind, Questions};

/// Part of the cache key; bump whenever the questions below change.
pub const QUESTION_SET_VERSION: &str = "1";

/// The seven yes/no concerns, in display order.
pub const BOOLEAN_CONCERNS: [&str; 7] = [
    "hardcoded_secret",
    "injection_risk",
    "touches_auth",
    "weakens_tests",
    "breaks_api",
    "data_migration",
    "debug_leftovers",
];

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
pub const PRIMARY_CONCERN: [(&str, &str); 10] = [
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
    ("logic", "behavior changes that fit none of the above"),
    ("none", "no notable risk: a mechanical or cosmetic change"),
    (
        "insufficient_evidence",
        "the hunk alone does not show enough to judge",
    ),
];

const PREAMBLE: &str = "The state describes one hunk of a code review diff. `hunk` is the \
numbered unified diff (`-` removed, `+` added); `enclosing` is the surrounding function \
before and after when known; `adds_tests` and `docs_only` are facts the tool computed from \
the file; `patterns` lists repeated mechanical edits already collapsed elsewhere in the diff.";

fn boolean(instructions: &str, yes: &str, no: &str) -> Question {
    Question {
        kind: QuestionKind::Boolean,
        instructions: format!("{PREAMBLE} {instructions}"),
        criteria: Some(json!({"true": yes, "false": no})),
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
    let criteria: serde_json::Map<String, Value> = PRIMARY_CONCERN
        .iter()
        .map(|(name, desc)| (name.to_string(), Value::String(desc.to_string())))
        .collect();
    q.insert(
        "primary_concern".into(),
        Question {
            kind: QuestionKind::Choice,
            instructions: format!(
                "{PREAMBLE} What is the single most important thing a reviewer should check in this hunk?"
            ),
            criteria: Some(Value::Object(criteria)),
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
        assert_eq!(q.len(), 10);
        let names: Vec<&str> = q.keys().map(String::as_str).collect();
        assert_eq!(&names[..7], &BOOLEAN_CONCERNS[..]);
        assert_eq!(
            &names[7..],
            &["blast_radius", "reviewer_effort", "primary_concern"]
        );
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
            10
        );
        assert!(v["primary_concern"]["criteria"]["insufficient_evidence"].is_string());
        assert!(
            v["touches_auth"]["instructions"]
                .as_str()
                .unwrap()
                .contains("`hunk`")
        );
    }

    #[test]
    fn rung_labels() {
        assert_eq!(rung_label(&BLAST_RADIUS, 0), "none");
        assert_eq!(rung_label(&BLAST_RADIUS, 3), "core");
        assert_eq!(rung_label(&REVIEWER_EFFORT, 1), "careful read");
        assert_eq!(rung_label(&REVIEWER_EFFORT, 9), "unknown");
    }
}
