//! Port of tests/test_grouping.py.

use indexmap::IndexMap;
use refactor_diff_core::warnings::{
    find_leftovers, inconsistent_renames, near_misses, still_defined,
};
use refactor_diff_core::{
    Group, Line, Signature, SignatureKind, Unit, Warning, WarningKind, analyzer_for, build_groups,
};

fn rename(old: &str, new: &str, detail: &str) -> Signature {
    Signature::new(
        SignatureKind::Rename,
        format!("rename\0{old}\0{new}"),
        old,
        new,
    )
    .with_detail(detail)
}

fn replace(old: &[&str], new: &[&str]) -> Signature {
    let key = ["replace"]
        .iter()
        .copied()
        .chain(old.iter().copied())
        .chain(["\x01"])
        .chain(new.iter().copied())
        .collect::<Vec<_>>()
        .join("\0");
    Signature::new(SignatureKind::Replace, key, old.join(" "), new.join(" "))
}

fn unit(uid: &str, path: &str, signatures: Vec<Signature>) -> Unit {
    Unit {
        id: uid.to_string(),
        path: path.to_string(),
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

fn map(units: Vec<Unit>) -> IndexMap<String, Unit> {
    units.into_iter().map(|u| (u.id.clone(), u)).collect()
}

fn near_for(units: Vec<Unit>) -> (IndexMap<String, Unit>, Vec<Group>, Vec<Warning>) {
    let mut units = map(units);
    let groups = build_groups(&mut units, 2);
    let warnings = near_misses(&mut units, &groups);
    (units, groups, warnings)
}

#[test]
fn min_count_threshold_and_explained() {
    let mut units = map(vec![
        unit("1", "a.py", vec![rename("f", "g", "call")]),
        unit("2", "b.py", vec![rename("f", "g", "call")]),
        unit("3", "c.py", vec![rename("x", "y", "call")]),
    ]);
    let groups = build_groups(&mut units, 2);
    let fg = groups.iter().find(|g| g.label == "f → g").unwrap();
    assert!(fg.mechanical);
    assert_eq!(fg.files, vec!["a.py", "b.py"]);
    assert!(
        !groups
            .iter()
            .find(|g| g.label == "x → y")
            .unwrap()
            .mechanical
    );
    let explained: Vec<bool> = units.values().map(|u| u.explained).collect();
    assert_eq!(explained, vec![true, true, false]);
}

#[test]
fn unit_needs_every_signature_mechanical() {
    let mut units = map(vec![
        unit("1", "a.py", vec![rename("f", "g", "call")]),
        unit(
            "2",
            "b.py",
            vec![rename("f", "g", "call"), rename("x", "y", "call")],
        ),
    ]);
    build_groups(&mut units, 2);
    let explained: Vec<bool> = units.values().map(|u| u.explained).collect();
    assert_eq!(explained, vec![true, false]);
}

#[test]
fn inconsistent_rename_warning_for_symbols() {
    let mut units = map(vec![
        unit("1", "a.py", vec![rename("f", "g", "definition")]),
        unit("2", "b.py", vec![rename("f", "h", "call")]),
    ]);
    let warnings = inconsistent_renames(&build_groups(&mut units, 1));
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].message.contains("f was renamed"));
    assert_eq!(warnings[0].kind, WarningKind::InconsistentRename);
}

#[test]
fn locals_renamed_differently_are_not_inconsistent() {
    let mut units = map(vec![
        unit("1", "a.py", vec![rename("user_id", "member_id", "name")]),
        unit("2", "b.py", vec![rename("user_id", "staff_id", "keyword")]),
    ]);
    assert!(inconsistent_renames(&build_groups(&mut units, 1)).is_empty());
}

#[test]
fn leftovers_found_in_code_not_comments_or_strings() {
    let mut units = map(vec![
        unit("1", "a.py", vec![rename("f", "g", "call")]),
        unit("2", "a.py", vec![rename("f", "g", "call")]),
    ]);
    let group = build_groups(&mut units, 2).remove(0);
    let py = analyzer_for("a.py").unwrap();
    let analysis = py.analyze("g()\n# f is gone\ns = 'f'\nf()\n");
    let files = std::collections::BTreeMap::from([("a.py".to_string(), &analysis)]);
    let warning = find_leftovers(&group, &files).expect("a warning");
    let locs: Vec<(String, u32)> = warning
        .locations
        .iter()
        .map(|l| (l.path.clone(), l.line))
        .collect();
    assert_eq!(locs, vec![("a.py".to_string(), 4)]);
    assert_eq!(warning.total, 1);
}

#[test]
fn no_leftovers_is_no_warning() {
    let mut units = map(vec![unit("1", "a.py", vec![rename("f", "g", "call")])]);
    let group = build_groups(&mut units, 1).remove(0);
    let py = analyzer_for("a.py").unwrap();
    let analysis = py.analyze("g()\n");
    let files = std::collections::BTreeMap::from([("a.py".to_string(), &analysis)]);
    assert!(find_leftovers(&group, &files).is_none());
}

#[test]
fn still_defined_cases() {
    let py = analyzer_for("a.py").unwrap();
    assert!(still_defined("f", &py.analyze("def f():\n    pass\n"), py));
    assert!(still_defined("C", &py.analyze("class C:\n    pass\n"), py));
    assert!(still_defined("X", &py.analyze("X: int = 1\n"), py));
    assert!(!still_defined("f", &py.analyze("f()\ny = f\n"), py));
    assert!(!still_defined(
        "x",
        &py.analyze("def g(x=1):\n    x = 2\n"),
        py
    ));
}

#[test]
fn near_miss_typo_rename() {
    let mut units: Vec<Unit> = (0..3)
        .map(|i| {
            unit(
                &i.to_string(),
                "a.py",
                vec![rename("get_user", "fetch_user", "call")],
            )
        })
        .collect();
    units.push(unit(
        "t",
        "b.py",
        vec![rename("get_user", "fetch_users", "call")],
    ));
    let (units, groups, warnings) = near_for(units);
    let mech: Vec<&Group> = groups.iter().filter(|g| g.mechanical).collect();
    assert_eq!(mech.len(), 1);
    let g = mech[0];
    let typo = &units["t"];
    let ids: Vec<&str> = typo.near.iter().map(|n| n.group_id.as_str()).collect();
    assert_eq!(ids, vec![g.id.as_str()]);
    assert!(typo.near[0].score >= 0.9);
    assert!(typo.near[0].hint.contains("fetch_users"));
    assert_eq!(warnings.len(), 1);
    let w = &warnings[0];
    assert_eq!(w.kind, WarningKind::NearMiss);
    assert_eq!(w.group_id.as_deref(), Some(g.id.as_str()));
    let locs: Vec<(String, u32)> = w
        .locations
        .iter()
        .map(|l| (l.path.clone(), l.line))
        .collect();
    assert_eq!(locs, vec![("b.py".to_string(), 1)]);
    assert!(
        units
            .values()
            .filter(|u| u.id != "t")
            .all(|u| u.near.is_empty())
    );
}

#[test]
fn unrelated_rename_is_not_a_near_miss() {
    let mut units: Vec<Unit> = (0..3)
        .map(|i| {
            unit(
                &i.to_string(),
                "a.py",
                vec![rename("get_user", "fetch_user", "call")],
            )
        })
        .collect();
    units.push(unit("o", "b.py", vec![rename("foo", "bar", "call")]));
    let (units, _, warnings) = near_for(units);
    assert!(units["o"].near.is_empty());
    assert!(warnings.is_empty());
}

#[test]
fn near_miss_template() {
    let tmpl = ["cfg", ".", "get", "(", "\x02", ")"];
    let mut units: Vec<Unit> = (0..2)
        .map(|i| {
            unit(
                &i.to_string(),
                "a.py",
                vec![replace(&tmpl, &["settings", ".", "\x02"])],
            )
        })
        .collect();
    units.push(unit(
        "t",
        "b.py",
        vec![replace(&tmpl, &["setting", ".", "\x02"])],
    ));
    let (units, _, _) = near_for(units);
    assert_eq!(units["t"].near.len(), 1);
}

#[test]
fn replace_never_matches_a_rename_group() {
    let mut units: Vec<Unit> = (0..3)
        .map(|i| {
            unit(
                &i.to_string(),
                "a.py",
                vec![rename("get_user", "fetch_user", "call")],
            )
        })
        .collect();
    units.push(unit(
        "o",
        "b.py",
        vec![replace(&["get_user"], &["fetch_user", "(", ")"])],
    ));
    let (units, _, _) = near_for(units);
    assert!(units["o"].near.is_empty());
}

#[test]
fn near_misses_are_capped_per_unit() {
    let mut units: Vec<Unit> = Vec::new();
    for i in 0..300 {
        for k in 0..2 {
            units.push(unit(
                &format!("{i}-{k}"),
                "a.py",
                vec![rename("name", &format!("name_{i:03}"), "call")],
            ));
        }
    }
    units.push(unit("t", "b.py", vec![rename("name", "name_9999", "call")]));
    let (units, _, _) = near_for(units);
    let n = units["t"].near.len();
    assert!((1..=2).contains(&n), "{n}");
}
