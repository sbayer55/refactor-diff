//! Token-level diff analysis that collapses repeated mechanical edits.
//!
//! The crate is pure: it takes already-loaded file contents and returns a [`Report`]. Git,
//! HTTP and everything else live in the server crate.

pub mod categories;
pub mod classify;
pub mod engine;
pub mod export;
pub mod fileview;
pub mod grouping;
pub mod hash;
pub mod hunks;
pub mod lang;
pub mod model;
pub mod moves;
pub mod seqmatch;
pub mod text;
pub mod verify;
pub mod warnings;

pub use categories::{Category, categorize};
pub use engine::{HeadFiles, NoHeadFiles, analyze};
pub use export::{ReviewMarks, anchor_line, markdown_summary};
pub use fileview::{FileDiff, FileRow, file_diff};
pub use grouping::build_groups;
pub use hash::short_hash;
pub use hunks::{CONTEXT, candidate_units, diff_hunks, line_matcher};
pub use lang::*;
pub use model::*;
pub use seqmatch::{Match, Opcode, SequenceMatcher, Tag};
pub use text::{Pos, char_col, char_len, split_lines};
