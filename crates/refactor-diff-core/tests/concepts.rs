//! The concept lens: detection, strip-equivalence, literal-normalized patterns, overrides.

mod common;

use common::{DirHeadFiles, changes_from, source};
use refactor_diff_core::{AnalyzeOptions, Concept, ConceptRole, Report, Unit, analyze_with};

const TYPES_OLD: &str = "\
export interface Event {
  id: string;
  programId?: string;
  title: string;
}
";
const TYPES_NEW: &str = "\
export interface Event {
  id: string;
  title: string;
}
";

const API_OLD: &str = "\
import { getProgram } from \"./programs\";
import { fetchEvents } from \"./events\";

export function load(siteId: string, programId: string) {
  return fetchEvents({ siteId, programId });
}
";
const API_NEW: &str = "\
import { fetchEvents } from \"./events\";

export function load(siteId: string) {
  return fetchEvents({ siteId });
}
";

const HOOK_OLD: &str = "\
export function useThing(siteId: string, programId: string) {
  useEffect(() => {
    if (!programId) {
      toaster.error(t(\"errors.noProgram\"));
      return;
    }
    refresh(siteId);
  }, [siteId, programId]);
}
";
const HOOK_NEW: &str = "\
export function useThing(siteId: string) {
  useEffect(() => {
    refresh(siteId);
  }, [siteId]);
}
";

/// Fixture records whose `programId` literals differ per file.
fn records(literal: Option<&str>) -> String {
    let mut out = String::from("export const events = [\n");
    for (id, title) in [("e1", "Intro"), ("e2", "Skills lab"), ("e3", "Debrief")] {
        out.push_str(&format!("  {{\n    id: \"{id}\",\n"));
        if let Some(lit) = literal {
            out.push_str(&format!("    programId: \"{lit}\",\n"));
        }
        out.push_str(&format!("    title: \"{title}\",\n  }},\n"));
    }
    out.push_str("];\n");
    out
}

const LIMITS_OLD: &str = "export const PAGE_LIMIT = 10;\n";
const LIMITS_NEW: &str = "export const PAGE_LIMIT = 20;\n";

fn analyze(before: &[(&str, &str)], after: &[(&str, &str)], concept: Option<&str>) -> Report {
    let opts = AnalyzeOptions {
        concept: concept.map(str::to_string),
    };
    let head = DirHeadFiles::from_files(after);
    analyze_with(
        source("main", "feature"),
        changes_from(before, after),
        2,
        &head,
        &opts,
    )
}

fn program_removal(concept: Option<&str>) -> Report {
    let (fx_a_old, fx_b_old, fx_new) = (
        records(Some("p1")),
        records(Some("prog-001")),
        records(None),
    );
    analyze(
        &[
            ("src/types.ts", TYPES_OLD),
            ("src/api.ts", API_OLD),
            ("src/hook.ts", HOOK_OLD),
            ("src/limits.ts", LIMITS_OLD),
            ("test/fixtures/a.ts", &fx_a_old),
            ("test/fixtures/b.ts", &fx_b_old),
        ],
        &[
            ("src/types.ts", TYPES_NEW),
            ("src/api.ts", API_NEW),
            ("src/hook.ts", HOOK_NEW),
            ("src/limits.ts", LIMITS_NEW),
            ("test/fixtures/a.ts", &fx_new),
            ("test/fixtures/b.ts", &fx_new),
        ],
        concept,
    )
}

fn units_in<'r>(report: &'r Report, path: &str) -> Vec<&'r Unit> {
    report.units.values().filter(|u| u.path == path).collect()
}

fn concept<'r>(report: &'r Report, stem: &str) -> &'r Concept {
    report
        .concepts
        .iter()
        .find(|c| c.stem == stem)
        .unwrap_or_else(|| panic!("no concept {stem}: {:?}", report.concepts))
}

#[test]
fn detects_the_removed_concept_and_explains_its_units() {
    let report = program_removal(None);
    assert_eq!(report.concepts[0].stem, "program", "{:?}", report.concepts);
    let c = &report.concepts[0];
    assert!(c.auto);
    assert!(c.score >= 0.15, "score {}", c.score);

    // Every unit outside the collateral file is the concept and nothing else.
    for u in report.units.values().filter(|u| u.path != "src/limits.ts") {
        assert!(
            c.explained_unit_ids.contains(&u.id),
            "not explained: {} {:?} → {:?}",
            u.path,
            u.old.iter().map(|l| &l.text).collect::<Vec<_>>(),
            u.new.iter().map(|l| &l.text).collect::<Vec<_>>(),
        );
    }
    // The collateral change neither mentions nor is explained by the concept.
    let collateral = units_in(&report, "src/limits.ts");
    assert_eq!(collateral.len(), 1);
    assert!(!c.explained_unit_ids.contains(&collateral[0].id));
    assert!(!c.touched_unit_ids.contains(&collateral[0].id));
    assert_eq!(c.files["src/limits.ts"].concept_lines, 0);
    assert_eq!(c.files["src/limits.ts"].collateral_lines, 2);
    assert!(c.files["src/hook.ts"].concept_lines > 0);
    assert_eq!(c.files["src/hook.ts"].collateral_lines, 0);

    // The overlay leaves the report's own accounting alone.
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["concepts"][0]["stem"], "program");
    assert_eq!(json["stats"]["units"], report.units.len());
}

#[test]
fn literal_variants_merge_into_one_pattern() {
    let report = program_removal(None);
    let c = &report.concepts[0];
    let fields: Vec<_> = c
        .patterns
        .iter()
        .filter(|p| p.label.starts_with("delete programId: <str>"))
        .collect();
    assert_eq!(fields.len(), 1, "{:#?}", c.patterns);
    let pattern = fields[0];
    assert_eq!(pattern.group_ids.len(), 2, "p1 and prog-001 groups merge");
    assert_eq!(pattern.role, ConceptRole::Fixture);
    assert!(pattern.norm_key.contains("<str>"));
    // Strongest pattern first: six fixture units.
    assert_eq!(c.patterns[0].norm_key, pattern.norm_key);
    for gid in &pattern.group_ids {
        let g = report.group(gid).expect("group");
        assert!(g.mechanical);
    }
}

#[test]
fn concept_override_pins_the_stem_and_changes_the_report_id() {
    let auto = program_removal(None);
    let pinned = program_removal(Some("Programs"));
    assert_ne!(auto.id, pinned.id);
    let c = concept(&pinned, "program");
    assert!(!c.auto, "asked for, so not auto");
    assert_eq!(pinned.concepts.len(), auto.concepts.len());
    assert_eq!(c.explained_unit_ids, auto.concepts[0].explained_unit_ids);

    // A stem the detector would not pick is added on top, weakest last.
    let extra = program_removal(Some("limit"));
    let limit = concept(&extra, "limit");
    assert!(!limit.auto);
    assert_eq!(extra.concepts.last().unwrap().stem, "limit");
    let collateral = units_in(&extra, "src/limits.ts");
    assert!(
        limit.touched_unit_ids.contains(&collateral[0].id),
        "PAGE_LIMIT's value changed, not just the name"
    );
}

#[test]
fn no_dominant_stem_means_no_auto_concepts() {
    let names = [
        "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "theta", "kappa",
    ];
    let before: String = names
        .iter()
        .map(|n| format!("export const {n} = 1;\n"))
        .collect();
    let after: String = names
        .iter()
        .map(|n| format!("export const {n} = 2;\n"))
        .collect();
    let report = analyze(&[("src/a.ts", &before)], &[("src/a.ts", &after)], None);
    assert!(report.concepts.is_empty(), "{:?}", report.concepts);
    let json = serde_json::to_value(&report).unwrap();
    assert!(json.get("concepts").is_none());
}

#[test]
fn guards_with_real_work_are_collateral() {
    let old = "\
export function save(programId: string) {
  if (!programId) {
    audit.record(\"missing\");
    return;
  }
  persist();
}
";
    let new = "\
export function save() {
  persist();
}
";
    let report = analyze(
        &[("src/save.ts", old)],
        &[("src/save.ts", new)],
        Some("program"),
    );
    let c = concept(&report, "program");
    let guard = report
        .units
        .values()
        .find(|u| u.old.iter().any(|l| l.text.contains("audit.record")))
        .expect("guard unit");
    assert!(
        c.touched_unit_ids.contains(&guard.id),
        "the guard did more than report an error"
    );
}

#[test]
fn renamed_identifiers_and_reworded_titles_count_as_the_concept() {
    let old = "\
import { SiteProgramPicker } from \"./SiteProgramPicker\";

it(\"renders sites without program chips\", () => {
  render(<SiteProgramPicker />);
});
";
    let new = "\
import { SitePicker } from \"./SitePicker\";

it(\"renders one row per site\", () => {
  render(<SitePicker />);
});
";
    let report = analyze(
        &[("src/picker.test.tsx", old)],
        &[("src/picker.test.tsx", new)],
        Some("program"),
    );
    let c = concept(&report, "program");
    let blank = |u: &&Unit| u.old.iter().chain(&u.new).all(|l| l.text.trim().is_empty());
    for u in report.units.values().filter(|u| !blank(u)) {
        assert!(
            c.explained_unit_ids.contains(&u.id),
            "not explained: {:?} → {:?}",
            u.old.iter().map(|l| &l.text).collect::<Vec<_>>(),
            u.new.iter().map(|l| &l.text).collect::<Vec<_>>(),
        );
    }
}
