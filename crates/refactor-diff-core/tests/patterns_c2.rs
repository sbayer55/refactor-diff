//! Cross-unit patterns: cascade cleanup, deleted test blocks and barrel edits after a
//! directory rename (workstream C2).

mod common;

use common::*;
use refactor_diff_core::{
    FileChange, FileStatus, Group, Report, SignatureKind, Unit, UnitTag, analyze,
};

fn unit_at<'r>(report: &'r Report, path: &str, old_start: u32) -> &'r Unit {
    report
        .units
        .values()
        .find(|u| u.path == path && u.old_start == old_start && !u.old.is_empty())
        .unwrap_or_else(|| panic!("no unit deleting from {path}:{old_start}"))
}

fn keys(u: &Unit) -> Vec<String> {
    u.signatures
        .iter()
        .map(|s| s.key.replace('\0', "|"))
        .collect()
}

fn group<'r>(report: &'r Report, key: &str) -> &'r Group {
    report
        .groups
        .iter()
        .find(|g| g.key == key)
        .unwrap_or_else(|| panic!("no group {key:?}"))
}

// ---------- cascade ----------

#[test]
fn import_dropped_with_its_last_use_is_cascade() {
    let before = "import { useMemo } from \"react\";
import { bar } from \"./bar\";

export const LIMIT = 3;

export function useDoubled(x: number) {
  return useMemo(() => x * 2, [x]);
}

export function keep() {
  return bar();
}
";
    let after = "import { bar } from \"./bar\";

export const LIMIT = 3;

export function keep() {
  return bar();
}
";
    let report = analyze_files(&[("src/a.ts", before)], &[("src/a.ts", after)]);
    let u = unit_at(&report, "src/a.ts", 1);
    assert_eq!(keys(u), ["cascade|useMemo"]);
    assert!(u.explained);
    let g = group(&report, "cascade\0useMemo");
    assert_eq!(g.kind, SignatureKind::Cascade);
    assert_eq!(g.label, "delete useMemo");
    assert!(g.mechanical);
    assert_eq!(g.details.get("import"), Some(&1));
    // The deleted function itself is the change to review.
    assert!(!unit_at(&report, "src/a.ts", 5).explained);
}

#[test]
fn name_dropped_from_an_import_list_is_cascade() {
    let before = "import { useMutation, useQueries } from \"@tanstack/react-query\";

export const LIMIT = 3;

export function useAll(ids: string[]) {
  return useQueries({ queries: ids.map((id) => ({ queryKey: [id] })) });
}

export function useSave() {
  return useMutation({});
}
";
    let after = "import { useMutation } from \"@tanstack/react-query\";

export const LIMIT = 3;

export function useSave() {
  return useMutation({});
}
";
    let report = analyze_files(&[("src/q.ts", before)], &[("src/q.ts", after)]);
    let u = unit_at(&report, "src/q.ts", 1);
    assert_eq!(keys(u), ["cascade|useQueries"]);
    assert!(u.explained);
}

#[test]
fn unused_hook_result_is_cascade_flagged_as_hook() {
    let before = "import { useMembers } from \"./members\";

export function useNames(id: string) {
  const map = new Map<string, string>();
  const membersQuery = useMembers(id);
  const other = 1;
  for (const m of membersQuery.data ?? []) {
    map.set(m.id, m.name);
  }
  return [map, other];
}
";
    let after = "export function useNames(id: string) {
  const map = new Map<string, string>();
  const other = 1;
  return [map, other];
}
";
    let report = analyze_files(&[("src/h.ts", before)], &[("src/h.ts", after)]);
    let decl = unit_at(&report, "src/h.ts", 5);
    assert_eq!(keys(decl), ["cascade|membersQuery"]);
    assert!(decl.explained);
    assert_eq!(
        group(&report, "cascade\0membersQuery").details.get("hook"),
        Some(&1)
    );
    // The import followed the declaration out.
    assert_eq!(
        keys(unit_at(&report, "src/h.ts", 1)),
        ["cascade|useMembers"]
    );
    // The loop changes what `map` holds: still for review.
    assert!(!unit_at(&report, "src/h.ts", 7).explained);
}

#[test]
fn name_still_used_at_head_is_not_cascade() {
    let before = "import { fmt } from \"./fmt\";
import { other } from \"./other\";

export const a = fmt(1);
export const b = other(1);
";
    let after = "import { other } from \"./other\";

export const b = other(fmt);
";
    let report = analyze_files(&[("src/n.ts", before)], &[("src/n.ts", after)]);
    assert!(
        report
            .units
            .values()
            .all(|u| u.signature_kinds().all(|k| k != SignatureKind::Cascade))
    );
}

#[test]
fn declaration_with_side_effects_is_not_cascade() {
    let before = "export function run(item: string) {
  const saved = save(item);
  const keep = 1;
  log(saved);
  return keep;
}
";
    let after = "export function run(item: string) {
  const keep = 1;
  return keep;
}
";
    let report = analyze_files(&[("src/s.ts", before)], &[("src/s.ts", after)]);
    let decl = unit_at(&report, "src/s.ts", 2);
    assert!(decl.signature_kinds().all(|k| k != SignatureKind::Cascade));
}

// ---------- test blocks ----------

const MATH_TEST: &str = "import { describe, it, expect } from \"vitest\";
import { add } from \"./add\";
import { sub } from \"./sub\";

describe(\"math\", () => {
  it(\"adds\", () => {
    expect(add(1, 2)).toBe(3);
  });

  it(\"subtracts\", () => {
    expect(sub(3, 2)).toBe(1);
  });
});
";

#[test]
fn deleted_it_block_is_tagged_and_labelled() {
    let after = "import { describe, it, expect } from \"vitest\";
import { add } from \"./add\";

describe(\"math\", () => {
  it(\"adds\", () => {
    expect(add(1, 2)).toBe(3);
  });
});
";
    let report = analyze_files(
        &[("src/math.test.ts", MATH_TEST)],
        &[("src/math.test.ts", after)],
    );
    let u = report
        .units
        .values()
        .find(|u| u.has_tag(UnitTag::TestRemoved))
        .expect("a test-removed unit");
    assert_eq!(keys(u), ["test|-subtracts"]);
    let sig = &u.signatures[0];
    assert_eq!(sig.kind, SignatureKind::Test);
    assert_eq!(sig.label(), "delete removed test: \"subtracts\"");
    assert_eq!(sig.detail, "imports:sub");
    // A single deleted test doesn't repeat: still for review, but filterable by its tag.
    assert!(!u.explained);
    // The import it alone used is cascade.
    assert_eq!(
        keys(unit_at(&report, "src/math.test.ts", 3)),
        ["cascade|sub"]
    );
}

#[test]
fn deleted_describe_with_nested_its_is_one_block() {
    let block = "
describe(\"programs\", () => {
  it(\"lists\", () => {
    expect(1).toBe(1);
  });

  it.each([1, 2])(\"handles %s\", (n) => {
    expect(n).toBeTruthy();
  });
});
";
    let base = "import { describe, it, expect } from \"vitest\";

describe(\"sites\", () => {
  it(\"lists\", () => {
    expect(2).toBe(2);
  });
});
";
    let with_block = format!("{base}{block}");
    let report = analyze_files(
        &[
            ("src/a.test.ts", &with_block),
            ("src/b.spec.ts", &with_block),
        ],
        &[("src/a.test.ts", base), ("src/b.spec.ts", base)],
    );
    let removed: Vec<&Unit> = report
        .units
        .values()
        .filter(|u| u.has_tag(UnitTag::TestRemoved))
        .collect();
    assert_eq!(removed.len(), 2);
    for u in &removed {
        assert_eq!(keys(u), ["test|-programs"]);
    }
    // The same block deleted twice repeats, so it collapses.
    let g = group(&report, "test\0-programs");
    assert!(g.mechanical);
    assert!(removed.iter().all(|u| u.explained));
}

#[test]
fn edited_test_is_not_removed() {
    let after = MATH_TEST.replace("it(\"subtracts\"", "it(\"subtracts numbers\"");
    let report = analyze_files(
        &[("src/math.test.ts", MATH_TEST)],
        &[("src/math.test.ts", &after)],
    );
    assert!(
        report
            .units
            .values()
            .all(|u| !u.has_tag(UnitTag::TestRemoved))
    );
}

// ---------- barrels ----------

fn change(
    status: FileStatus,
    old_path: Option<&str>,
    path: &str,
    old: &str,
    new: &str,
) -> FileChange {
    FileChange {
        path: path.to_string(),
        old_path: old_path.map(String::from),
        status,
        old_text: old.to_string(),
        new_text: new.to_string(),
    }
}

#[test]
fn barrel_edits_join_the_directory_rename() {
    let comp = |name: &str| format!("export function {name}() {{\n  return null;\n}}\n");
    let stories = |name: &str| {
        format!(
            "import {{ {name} }} from \"./{name}\";\n\nexport default {{ component: {name} }};\n"
        )
    };
    let changes = vec![
        change(
            FileStatus::Renamed,
            Some("src/layout/OldPicker/OldPicker.tsx"),
            "src/layout/NewPicker/NewPicker.tsx",
            &comp("OldPicker"),
            &comp("NewPicker"),
        ),
        change(
            FileStatus::Renamed,
            Some("src/layout/OldPicker/OldPicker.stories.tsx"),
            "src/layout/NewPicker/NewPicker.stories.tsx",
            &stories("OldPicker"),
            &stories("NewPicker"),
        ),
        change(
            FileStatus::Deleted,
            None,
            "src/layout/OldPicker/index.ts",
            "export * from \"./OldPicker\";\n",
            "",
        ),
        change(
            FileStatus::Added,
            None,
            "src/layout/NewPicker/index.ts",
            "",
            "export * from \"./NewPicker\";\n",
        ),
        change(
            FileStatus::Modified,
            None,
            "src/layout/index.ts",
            "export * from \"./Footer\";\nexport * from \"./OldPicker\";\n",
            "export * from \"./Footer\";\nexport * from \"./NewPicker\";\n",
        ),
    ];
    let head = DirHeadFiles::from_files(&[]);
    let report = analyze(source("main", "feature"), changes, 2, &head);

    let key = "rename\0OldPicker\0NewPicker";
    for path in [
        "src/layout/OldPicker/index.ts",
        "src/layout/NewPicker/index.ts",
        "src/layout/index.ts",
    ] {
        let u = report
            .units
            .values()
            .find(|u| u.path == path)
            .unwrap_or_else(|| panic!("no unit in {path}"));
        assert_eq!(keys(u), ["rename|OldPicker|NewPicker"], "{path}");
        assert!(u.has_tag(UnitTag::FileMove), "{path}");
        assert!(u.explained, "{path}");
    }
    let g = group(&report, key);
    assert!(g.mechanical);
    assert_eq!(g.details.get("barrel"), Some(&3));
    // The identifier renames in the moved files share the group.
    assert!(g.files.iter().any(|f| f.ends_with("NewPicker.tsx")));
}
