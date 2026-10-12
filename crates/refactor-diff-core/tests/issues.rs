//! Code issues (`issues.rs`): syntax errors, unreachable code and unused names.

use refactor_diff_core::{Pos, Severity, find_issues};

/// `(code, line, underlined text)` for each issue, in order.
fn issues(path: &str, src: &str) -> Vec<(&'static str, u32, String)> {
    let lines: Vec<&str> = src.lines().collect();
    find_issues(path, src)
        .into_iter()
        .map(|i| {
            let text = if i.start.line == i.end.line {
                let line = lines[i.start.line as usize - 1];
                line.chars()
                    .skip(i.start.col as usize)
                    .take((i.end.col - i.start.col) as usize)
                    .collect()
            } else {
                format!("…{}", i.end.line)
            };
            (i.code, i.start.line, text)
        })
        .collect()
}

fn s(code: &'static str, line: u32, text: &str) -> (&'static str, u32, String) {
    (code, line, text.to_string())
}

#[test]
fn unsupported_language_has_no_issues() {
    assert!(find_issues("README.md", "x = (").is_empty());
}

#[test]
fn python_unused_locals() {
    let src = "\
def f(a):
    tmp = 1
    x, y = pair()
    for i in range(3):
        pass
    with open(a) as fh:
        pass
    try:
        pass
    except ValueError as e:
        pass
    _ignored = 2
    used = 3
    return used + y
";
    assert_eq!(
        issues("m.py", src),
        vec![
            s("unused-variable", 2, "tmp"),
            s("unused-variable", 3, "x"),
            s("unused-variable", 4, "i"),
            s("unused-variable", 6, "fh"),
            s("unused-variable", 10, "e"),
        ]
    );
}

#[test]
fn python_closures_globals_and_augmented_assignment_count_as_use() {
    let src = "\
def outer():
    seen = []
    count = 0
    def inner():
        return seen
    count += 1
    return inner

def g():
    global STATE
    STATE = 1

def h():
    name = 1
    return obj.name, call(name=2)
";
    assert_eq!(issues("m.py", src), vec![s("unused-variable", 14, "name")]);
}

#[test]
fn python_unused_parameters() {
    let src = "\
class C:
    def method(self, used, unused, *args, _skip=1, **kwargs):
        return used

    def __exit__(self, exc_type, exc, tb):
        return None

    @abstractmethod
    def abstract(self, a):
        pass

    def stub(self, a):
        ...

    @classmethod
    def make(cls, a):
        return 1
";
    assert_eq!(
        issues("m.py", src),
        vec![
            s("unused-parameter", 2, "unused"),
            s("unused-parameter", 16, "a")
        ]
    );
}

#[test]
fn python_unused_imports() {
    let src = "\
from __future__ import annotations
import os
import os.path as osp
import sys
from typing import List, Dict as D
from pkg import thing as thing
from typing import Optional

__all__ = [\"Dict\"]

def f(x: \"Optional[int]\") -> List[int]:
    return sys.argv, x
";
    assert_eq!(
        issues("m.py", src),
        vec![
            s("unused-import", 2, "os"),
            s("unused-import", 3, "osp"),
            s("unused-import", 5, "D"),
        ]
    );
    assert!(issues("pkg/__init__.py", src).is_empty());
}

#[test]
fn python_unreachable_code() {
    let src = "\
def f(x):
    if x:
        return 1
        print(x)
    while x:
        break
        x -= 1
        x -= 2
    return 2
    # trailing comment
";
    assert_eq!(
        issues("m.py", src),
        vec![s("unreachable", 4, "print(x)"), s("unreachable", 7, "…8")]
    );
}

#[test]
fn syntax_errors_suppress_other_checks() {
    let found = find_issues("m.py", "import os\ndef f(:\n    tmp = 1\n");
    assert!(!found.is_empty());
    assert!(
        found
            .iter()
            .all(|i| i.code == "syntax-error" && i.severity == Severity::Error)
    );
}

#[test]
fn severities() {
    let src = "import os\ndef f(a):\n    b = 1\n    return\n    c()\n";
    let sev: Vec<_> = find_issues("m.py", src)
        .into_iter()
        .map(|i| (i.code, i.severity))
        .collect();
    assert_eq!(
        sev,
        vec![
            ("unused-import", Severity::Info),
            ("unused-parameter", Severity::Info),
            ("unused-variable", Severity::Warning),
            ("unreachable", Severity::Warning),
        ]
    );
}

#[test]
fn ts_unused_locals_and_parameters() {
    let src = "\
export function f(a: number, b: number, c: number) {
  const tmp = 1;
  let { x, y } = obj;
  const { omit, ...rest } = obj;
  const [first, second] = pair;
  for (const item of items) {}
  const used = () => b + y + second + rest.z;
  return used;
}

class K {
  constructor(private readonly svc: Service, unused: number) {}
  handle(_e: Event, i: number) { return [1].map((v, idx) => idx); }
}
const h = (_e: Event, i: number) => [1].map((v, idx) => idx);
class L {
}
";
    assert_eq!(
        issues("m.ts", src),
        vec![
            s("unused-parameter", 1, "c"),
            s("unused-variable", 2, "tmp"),
            s("unused-variable", 3, "x"),
            s("unused-variable", 5, "first"),
            s("unused-variable", 6, "item"),
            s("unused-parameter", 15, "i"),
        ]
    );
}

#[test]
fn ts_unused_imports() {
    let src = "\
import express from \"express\";
import * as path from \"path\";
import { A, B as Bee, type C, D } from \"./m\";
import \"./side-effect\";
import React from \"react\";

/** @param x {D} */
export { A };
export function f(x: C) { return x && <div />; }
";
    assert_eq!(
        issues("m.tsx", src),
        vec![
            s("unused-import", 1, "express"),
            s("unused-import", 2, "path"),
            s("unused-import", 3, "Bee"),
        ]
    );
}

#[test]
fn ts_unreachable_code_skips_hoisted_declarations() {
    let src = "\
function f(x) {
  switch (x) {
    case 1:
      return 1;
      x++;
    default:
      throw new Error();
  }
  return g();
  function g() { return 1; }
  console.log(x);
}
";
    assert_eq!(
        issues("m.js", src),
        vec![
            s("unreachable", 5, "x++;"),
            s("unreachable", 11, "console.log(x);")
        ]
    );
}

#[test]
fn missing_tokens_get_a_visible_range() {
    let found = find_issues("m.ts", "const x = (1 + 2;\n");
    assert!(!found.is_empty());
    assert!(
        found
            .iter()
            .all(|i| i.start < i.end || i.start.line < i.end.line)
    );
    let _ = Pos::new(1, 0);
}
