//! Whole-file views of one changed file, for showing context and the old/new versions.

use std::collections::HashMap;

use serde::Serialize;

use crate::hunks::line_matcher;
use crate::model::{Category, FileStatus, LineType, Report};
use crate::seqmatch::Tag;
use crate::text::split_lines;

/// One row of the whole-file diff. Context rows carry no `unit`/`hl` keys; changed rows always
/// do, with `unit: null` when the line belongs to no unit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FileRow {
    pub t: LineType,
    pub o: Option<u32>,
    pub n: Option<u32>,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hl: Option<Vec<[u32; 2]>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FileDiff {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub category: Category,
    pub analyzed: bool,
    pub old_lines: usize,
    pub new_lines: usize,
    pub lines: Vec<FileRow>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("That file is not part of this diff.")]
    UnknownPath,
}

/// Every line of the file as a unified diff (no context cut-off).
///
/// Uses the same line matcher as the engine, so changed lines carry the ids of the units they
/// belong to plus their token highlights. The old file is the rows without `+`, the new file
/// the rows without `-`.
#[allow(clippy::needless_range_loop)] // indices are line numbers
pub fn file_diff(report: &Report, path: &str) -> Result<FileDiff, Error> {
    let summary = report.file(path).ok_or(Error::UnknownPath)?;
    let (old_text, new_text) = report.texts.get(path).ok_or(Error::UnknownPath)?;
    let (old, new) = (split_lines(old_text), split_lines(new_text));

    let mut by_old: HashMap<u32, (&str, &[[u32; 2]])> = HashMap::new();
    let mut by_new: HashMap<u32, (&str, &[[u32; 2]])> = HashMap::new();
    for u in report.units.values().filter(|u| u.path == path) {
        for (k, ln) in u.old.iter().enumerate() {
            by_old.insert(u.old_start + k as u32, (&u.id, &ln.hl));
        }
        for (k, ln) in u.new.iter().enumerate() {
            by_new.insert(u.new_start + k as u32, (&u.id, &ln.hl));
        }
    }

    let mut lines = Vec::new();
    for op in line_matcher(&old, &new).opcodes() {
        if op.tag == Tag::Equal {
            for i in op.i1..op.i2 {
                lines.push(FileRow {
                    t: LineType::Context,
                    o: Some(i as u32 + 1),
                    n: Some((op.j1 + (i - op.i1)) as u32 + 1),
                    text: old[i].clone(),
                    unit: None,
                    hl: None,
                });
            }
            continue;
        }
        for i in op.i1..op.i2 {
            let (unit, hl) = by_old
                .get(&(i as u32 + 1))
                .map_or((None, &[][..]), |(u, hl)| (Some(u.to_string()), *hl));
            lines.push(FileRow {
                t: LineType::Removed,
                o: Some(i as u32 + 1),
                n: None,
                text: old[i].clone(),
                unit: Some(unit),
                hl: Some(hl.to_vec()),
            });
        }
        for j in op.j1..op.j2 {
            let (unit, hl) = by_new
                .get(&(j as u32 + 1))
                .map_or((None, &[][..]), |(u, hl)| (Some(u.to_string()), *hl));
            lines.push(FileRow {
                t: LineType::Added,
                o: None,
                n: Some(j as u32 + 1),
                text: new[j].clone(),
                unit: Some(unit),
                hl: Some(hl.to_vec()),
            });
        }
    }

    Ok(FileDiff {
        path: path.to_string(),
        old_path: summary.old_path.clone(),
        status: summary.status,
        category: summary.category,
        analyzed: summary.analyzed,
        old_lines: old.len(),
        new_lines: new.len(),
        lines,
    })
}
