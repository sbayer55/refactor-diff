//! Port of tests/test_filters.py: tags behind the import, file-rename and moved-function
//! filters.

mod common;

use common::*;
use refactor_diff_core::{FileChange, FileStatus, Report, SignatureKind, Unit, UnitTag, analyze};

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

const TS_HELPER: &str = concat!(
    "export function helper(x: number, y: number): number {\n  const total = x + y;\n",
    "  if (total > 10) {\n    return total * 2;\n  }\n  return total;\n}\n"
);

/// `analyze(repo, "main", "feature")` over an in-memory repository: `after` is the whole head
/// tree (a file missing from it was deleted). Unchanged files are left out, as `git diff`
/// would.
fn report_for(before: &[(&str, &str)], after: &[(&str, &str)]) -> Report {
    let mut changes = changes_from(before, after);
    changes.retain(|c| c.status != FileStatus::Modified || c.old_text != c.new_text);
    analyze_changes(changes, after)
}

fn analyze_changes(changes: Vec<FileChange>, after: &[(&str, &str)]) -> Report {
    let head = DirHeadFiles::from_files(after);
    analyze(source("main", "feature"), changes, 2, &head)
}

/// A file git's `-M` detection reports as renamed even though its content changed.
fn renamed(old_path: &str, new_path: &str, old_text: &str, new_text: &str) -> FileChange {
    FileChange {
        path: new_path.to_string(),
        old_path: Some(old_path.to_string()),
        status: FileStatus::Renamed,
        old_text: old_text.to_string(),
        new_text: new_text.to_string(),
    }
}

fn single<T>(items: Vec<T>) -> T {
    assert_eq!(items.len(), 1, "expected exactly one item");
    items.into_iter().next().unwrap()
}

fn tagged(report: &Report, tag: UnitTag) -> Vec<&Unit> {
    report.units.values().filter(|u| u.has_tag(tag)).collect()
}

fn move_units(report: &Report) -> Vec<&Unit> {
    let g = single(
        report
            .groups
            .iter()
            .filter(|g| g.kind == SignatureKind::Move)
            .collect(),
    );
    g.unit_ids.iter().map(|u| &report.units[u]).collect()
}

fn paths(units: &[&Unit]) -> Vec<String> {
    units.iter().map(|u| u.path.clone()).collect()
}

// --- imports ---------------------------------------------------------------------------------

#[test]
fn import_only_edits_are_tagged() {
    let report = report_for(
        &[("a.py", "import os\nimport sys\n\nprint(os, sys)\n")],
        &[(
            "a.py",
            "import sys\nimport os\nimport re\n\nprint(os, sys)\n",
        )],
    );
    assert!(!report.units.is_empty());
    assert!(report.units.values().all(|u| u.has_tag(UnitTag::Imports)));
}

#[test]
fn import_mixed_with_code_is_not_tagged() {
    let report = report_for(
        &[("a.py", "import os; x = 1\n")],
        &[("a.py", "import re; x = 2\n")],
    );
    assert!(tagged(&report, UnitTag::Imports).is_empty());
}

#[test]
fn code_change_is_not_tagged_as_import() {
    let report = report_for(
        &[("a.py", "import os\n\nx = 1\n")],
        &[("a.py", "import os\n\nx = 2\n")],
    );
    assert!(tagged(&report, UnitTag::Imports).is_empty());
}

#[test]
fn typescript_import_edits_are_tagged() {
    let report = report_for(
        &[(
            "a.ts",
            "import { a } from \"./m\";\n\nexport const x = a;\n",
        )],
        &[(
            "a.ts",
            "import { a, b } from \"./m\";\n\nexport const x = a;\n",
        )],
    );
    let u = single(report.units.values().collect());
    assert!(u.has_tag(UnitTag::Imports));
    let g = single(report.groups.iter().collect());
    assert_eq!(g.kind, SignatureKind::Import);
    assert_eq!(g.label, "insert import { b } from \"./m\"");
}

// --- file renames ----------------------------------------------------------------------------

#[test]
fn pure_rename_and_its_import_updates() {
    let models = "class User:\n    pass\n";
    let report = report_for(
        &[
            ("pkg/models.py", models),
            ("pkg/api.py", "from pkg.models import User\n\nprint(User)\n"),
        ],
        &[
            ("pkg/entities.py", models),
            (
                "pkg/api.py",
                "from pkg.entities import User\n\nprint(User)\n",
            ),
        ],
    );
    assert!(report.file("pkg/entities.py").unwrap().pure_rename);
    assert!(!report.file("pkg/api.py").unwrap().pure_rename);
    let u = single(tagged(&report, UnitTag::FileMove));
    assert_eq!(u.path, "pkg/api.py");
}

#[test]
fn relative_import_after_rename() {
    let models = "class User:\n    pass\n";
    let report = report_for(
        &[
            ("pkg/models.py", models),
            ("pkg/api.py", "from .models import User\n"),
        ],
        &[
            ("pkg/entities.py", models),
            ("pkg/api.py", "from .entities import User\n"),
        ],
    );
    assert_eq!(
        paths(&tagged(&report, UnitTag::FileMove)),
        vec!["pkg/api.py"]
    );
}

#[test]
fn moved_importer_keeps_its_relative_target() {
    let body = "from .models import User\n\n\ndef make():\n    return User()\n";
    let moved = body.replace("from .models", "from ..models");
    // git's rename detection pairs pkg/api.py with pkg/sub/api.py despite the edit.
    let report = analyze_changes(
        vec![renamed("pkg/api.py", "pkg/sub/api.py", body, &moved)],
        &[
            ("pkg/models.py", "class User:\n    pass\n"),
            ("pkg/sub/api.py", &moved),
        ],
    );
    let f = single(
        report
            .files
            .iter()
            .filter(|f| f.path == "pkg/sub/api.py")
            .collect(),
    );
    assert_eq!(f.status, FileStatus::Renamed);
    assert!(!f.pure_rename);
    let u = single(tagged(&report, UnitTag::FileMove));
    assert_eq!(u.path, "pkg/sub/api.py");
}

#[test]
fn import_of_a_different_module_is_not_a_file_move() {
    let report = report_for(
        &[
            ("a.py", "from pkg.models import User\n"),
            ("pkg/models.py", "class User:\n    pass\n"),
        ],
        &[
            ("a.py", "from pkg.other import User\n"),
            ("pkg/models.py", "class User:\n    pass\n"),
        ],
    );
    assert!(!tagged(&report, UnitTag::Imports).is_empty());
    assert!(tagged(&report, UnitTag::FileMove).is_empty());
}

#[test]
fn edits_inside_a_renamed_file_are_not_tagged() {
    let before =
        "import os\n\n\ndef f():\n    return os.sep\n\n\ndef g():\n    return 1\n".repeat(3);
    let after = before.replacen("return 1", "return 2", 1);
    // git's rename detection pairs a.py with b.py despite the edit.
    let report = analyze_changes(
        vec![renamed("a.py", "b.py", &before, &after)],
        &[("b.py", &after)],
    );
    let f = single(report.files.iter().collect());
    assert_eq!(f.status, FileStatus::Renamed);
    assert!(!f.pure_rename);
    assert!(!report.units.is_empty());
    assert!(!report.units.values().any(|u| !u.tags.is_empty()));
}

#[test]
fn typescript_specifier_after_rename() {
    let models = "export class User {}\n";
    let report = report_for(
        &[
            ("src/models.ts", models),
            ("src/api.ts", "import { User } from \"./models\";\n"),
        ],
        &[
            ("src/entities/index.ts", models),
            ("src/api.ts", "import { User } from \"./entities\";\n"),
        ],
    );
    assert_eq!(
        paths(&tagged(&report, UnitTag::FileMove)),
        vec!["src/api.ts"]
    );
}

// --- moved functions -------------------------------------------------------------------------

#[test]
fn exact_move_is_tagged_with_its_import() {
    let report = report_for(
        &[
            ("pkg/a.py", &format!("{HELPER}\n\n{OTHER}")),
            ("pkg/b.py", "X = 1\n"),
            (
                "pkg/c.py",
                "from pkg.a import helper\n\nprint(helper(1, 2))\n",
            ),
        ],
        &[
            ("pkg/a.py", OTHER),
            ("pkg/b.py", &format!("X = 1\n\n\n{HELPER}")),
            (
                "pkg/c.py",
                "from pkg.b import helper\n\nprint(helper(1, 2))\n",
            ),
        ],
    );
    let units = move_units(&report);
    assert_eq!(units.len(), 3);
    assert!(units.iter().all(|u| u.has_tag(UnitTag::Moved)));
}

#[test]
fn move_with_an_edit_is_not_certain() {
    let edited = HELPER.replace("total * 2", "total * 3");
    let report = report_for(
        &[
            ("a.py", &format!("{HELPER}\n\n{OTHER}")),
            ("b.py", "X = 1\n"),
        ],
        &[("a.py", OTHER), ("b.py", &format!("X = 1\n\n\n{edited}"))],
    );
    assert!(!move_units(&report).is_empty());
    assert!(tagged(&report, UnitTag::Moved).is_empty());
}

#[test]
fn move_with_an_edited_comment_is_not_certain() {
    let commented = HELPER.replace("    return total\n", "    return total  # done\n");
    let report = report_for(
        &[
            ("a.py", &format!("{HELPER}\n\n{OTHER}")),
            ("b.py", "X = 1\n"),
        ],
        &[
            ("a.py", OTHER),
            ("b.py", &format!("X = 1\n\n\n{commented}")),
        ],
    );
    assert!(!move_units(&report).is_empty());
    assert!(tagged(&report, UnitTag::Moved).is_empty());
}

#[test]
fn duplicated_code_is_not_certain() {
    // Deleted from two files, inserted once: which copy moved is a guess.
    let report = report_for(
        &[
            ("a.py", &format!("{HELPER}\n\n{OTHER}")),
            ("b.py", &format!("{HELPER}\n\n{OTHER}")),
            ("c.py", "X = 1\n"),
        ],
        &[
            ("a.py", OTHER),
            ("b.py", OTHER),
            ("c.py", &format!("X = 1\n\n\n{HELPER}")),
        ],
    );
    assert!(!move_units(&report).is_empty());
    assert!(tagged(&report, UnitTag::Moved).is_empty());
}

#[test]
fn method_moved_to_another_class_is_not_certain() {
    let method = concat!(
        "    def area(self):\n        w = self.width\n        h = self.height\n",
        "        return w * h\n"
    );
    let report = report_for(
        &[
            ("a.py", &format!("class A:\n    x = 1\n\n{method}")),
            ("b.py", "class B:\n    y = 2\n"),
        ],
        &[
            ("a.py", "class A:\n    x = 1\n"),
            ("b.py", &format!("class B:\n    y = 2\n\n{method}")),
        ],
    );
    assert!(!move_units(&report).is_empty());
    assert!(tagged(&report, UnitTag::Moved).is_empty());
}

#[test]
fn loose_statements_are_not_certain() {
    let block =
        "config = load()\nconfig.update(extra)\nconfig.validate(strict=True)\nrun(config)\n";
    let report = report_for(
        &[
            ("a.py", &format!("{block}\n\n{OTHER}")),
            ("b.py", "X = 1\n"),
        ],
        &[("a.py", OTHER), ("b.py", &format!("X = 1\n\n\n{block}"))],
    );
    assert!(!move_units(&report).is_empty());
    assert!(tagged(&report, UnitTag::Moved).is_empty());
}

#[test]
fn typescript_function_move_is_certain() {
    let report = report_for(
        &[
            ("a.ts", &format!("{TS_HELPER}\nexport const X = 1;\n")),
            ("b.ts", "export const Y = 2;\n"),
            ("c.ts", "import { helper } from \"./a\";\n\nhelper(1, 2);\n"),
        ],
        &[
            ("a.ts", "export const X = 1;\n"),
            ("b.ts", &format!("export const Y = 2;\n\n{TS_HELPER}")),
            ("c.ts", "import { helper } from \"./b\";\n\nhelper(1, 2);\n"),
        ],
    );
    let units = move_units(&report);
    assert_eq!(units.len(), 3); // block out, block in, import in c.ts
    assert!(units.iter().all(|u| u.has_tag(UnitTag::Moved)));
}
