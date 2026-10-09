//! A port of CPython's `difflib.SequenceMatcher` (without the `autojunk` heuristic, which no
//! call site enables).
//!
//! Every id, hunk boundary, grouping threshold and move score in a report depends on the exact
//! alignment this algorithm produces, so it is transcribed from `Lib/difflib.py` rather than
//! replaced by a Myers or patience diff. The junk predicate works the way `isjunk` does: an
//! element is junk when the predicate holds for it as it occurs in `b`; junk never anchors a
//! match but can extend one.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

/// What an opcode does to turn `a[i1..i2]` into `b[j1..j2]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tag {
    Equal,
    Replace,
    Delete,
    Insert,
}

impl Tag {
    pub const fn as_str(self) -> &'static str {
        match self {
            Tag::Equal => "equal",
            Tag::Replace => "replace",
            Tag::Delete => "delete",
            Tag::Insert => "insert",
        }
    }
}

/// `(tag, i1, i2, j1, j2)`: 0-based half-open slices of `a` and `b`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Opcode {
    pub tag: Tag,
    pub i1: usize,
    pub i2: usize,
    pub j1: usize,
    pub j2: usize,
}

impl Opcode {
    pub const fn new(tag: Tag, i1: usize, i2: usize, j1: usize, j2: usize) -> Self {
        Self {
            tag,
            i1,
            i2,
            j1,
            j2,
        }
    }

    /// The 1-based inclusive line range on the old side, or `None` when nothing was removed.
    pub fn old_range(&self) -> Option<(u32, u32)> {
        (self.i2 > self.i1).then(|| (self.i1 as u32 + 1, self.i2 as u32))
    }

    /// The 1-based inclusive line range on the new side, or `None` when nothing was added.
    pub fn new_range(&self) -> Option<(u32, u32)> {
        (self.j2 > self.j1).then(|| (self.j1 as u32 + 1, self.j2 as u32))
    }
}

/// `a[a..a+size] == b[b..b+size]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Match {
    pub a: usize,
    pub b: usize,
    pub size: usize,
}

pub struct SequenceMatcher<'a, T> {
    a: &'a [T],
    b: &'a [T],
    /// Positions in `b` of each non-junk element, ascending.
    b2j: HashMap<&'a T, Vec<usize>>,
    /// Junk elements of `b` (by value), as `isjunk` classifies them.
    bjunk: HashSet<&'a T>,
    blocks: OnceCell<Vec<Match>>,
    fullbcount: OnceCell<HashMap<&'a T, usize>>,
}

impl<'a, T: Hash + Eq> SequenceMatcher<'a, T> {
    /// A matcher with no junk.
    pub fn new(a: &'a [T], b: &'a [T]) -> Self {
        Self::with_junk(a, b, |_| false)
    }

    /// A matcher whose `is_junk` elements never anchor a match (Python's `isjunk`).
    pub fn with_junk(a: &'a [T], b: &'a [T], is_junk: impl Fn(&T) -> bool) -> Self {
        let mut b2j: HashMap<&'a T, Vec<usize>> = HashMap::new();
        for (j, elt) in b.iter().enumerate() {
            b2j.entry(elt).or_default().push(j);
        }
        let bjunk: HashSet<&'a T> = b2j.keys().copied().filter(|elt| is_junk(elt)).collect();
        for elt in &bjunk {
            b2j.remove(elt);
        }
        Self {
            a,
            b,
            b2j,
            bjunk,
            blocks: OnceCell::new(),
            fullbcount: OnceCell::new(),
        }
    }

    /// The longest matching block in `a[alo..ahi]` and `b[blo..bhi]`, with CPython's
    /// tie-breaking (earliest in `a`, then earliest in `b`) and junk extension.
    pub fn find_longest_match(&self, alo: usize, ahi: usize, blo: usize, bhi: usize) -> Match {
        let (a, b) = (self.a, self.b);
        let is_junk = |j: usize| self.bjunk.contains(&b[j]);
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);

        // j2len[j] = length of the longest match ending at a[i-1], b[j-1] from the previous
        // row; stored at index j+1 so that "j-1" for j == 0 reads the (always zero) slot 0.
        let mut j2len = vec![0usize; b.len() + 1];
        let mut newj2len = vec![0usize; b.len() + 1];
        let mut touched: Vec<usize> = Vec::new();
        let mut newtouched: Vec<usize> = Vec::new();
        for (i, ai) in a.iter().enumerate().take(ahi).skip(alo) {
            if let Some(js) = self.b2j.get(ai) {
                for &j in js {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let k = j2len[j] + 1;
                    newj2len[j + 1] = k;
                    newtouched.push(j + 1);
                    if k > bestsize {
                        besti = i + 1 - k;
                        bestj = j + 1 - k;
                        bestsize = k;
                    }
                }
            }
            for &t in &touched {
                j2len[t] = 0;
            }
            std::mem::swap(&mut j2len, &mut newj2len);
            std::mem::swap(&mut touched, &mut newtouched);
            newtouched.clear();
        }

        // Extend with non-junk, then with junk, exactly in CPython's order.
        while besti > alo && bestj > blo && !is_junk(bestj - 1) && a[besti - 1] == b[bestj - 1] {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi
            && bestj + bestsize < bhi
            && !is_junk(bestj + bestsize)
            && a[besti + bestsize] == b[bestj + bestsize]
        {
            bestsize += 1;
        }
        while besti > alo && bestj > blo && is_junk(bestj - 1) && a[besti - 1] == b[bestj - 1] {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi
            && bestj + bestsize < bhi
            && is_junk(bestj + bestsize)
            && a[besti + bestsize] == b[bestj + bestsize]
        {
            bestsize += 1;
        }
        Match {
            a: besti,
            b: bestj,
            size: bestsize,
        }
    }

    /// Non-overlapping matching blocks in increasing order, ending with the sentinel
    /// `Match { a: len(a), b: len(b), size: 0 }`.
    pub fn matching_blocks(&self) -> &[Match] {
        self.blocks.get_or_init(|| {
            let (la, lb) = (self.a.len(), self.b.len());
            let mut queue = vec![(0, la, 0, lb)];
            let mut blocks = Vec::new();
            while let Some((alo, ahi, blo, bhi)) = queue.pop() {
                let m = self.find_longest_match(alo, ahi, blo, bhi);
                if m.size > 0 {
                    blocks.push(m);
                    if alo < m.a && blo < m.b {
                        queue.push((alo, m.a, blo, m.b));
                    }
                    if m.a + m.size < ahi && m.b + m.size < bhi {
                        queue.push((m.a + m.size, ahi, m.b + m.size, bhi));
                    }
                }
            }
            blocks.sort();
            let mut merged: Vec<Match> = Vec::new();
            let (mut i1, mut j1, mut k1) = (0, 0, 0);
            for m in blocks {
                if i1 + k1 == m.a && j1 + k1 == m.b {
                    k1 += m.size;
                } else {
                    if k1 > 0 {
                        merged.push(Match {
                            a: i1,
                            b: j1,
                            size: k1,
                        });
                    }
                    (i1, j1, k1) = (m.a, m.b, m.size);
                }
            }
            if k1 > 0 {
                merged.push(Match {
                    a: i1,
                    b: j1,
                    size: k1,
                });
            }
            merged.push(Match {
                a: la,
                b: lb,
                size: 0,
            });
            merged
        })
    }

    /// How to turn `a` into `b`, as a list of opcodes covering both sequences.
    pub fn opcodes(&self) -> Vec<Opcode> {
        let (mut i, mut j) = (0, 0);
        let mut out = Vec::new();
        for m in self.matching_blocks() {
            let tag = if i < m.a && j < m.b {
                Some(Tag::Replace)
            } else if i < m.a {
                Some(Tag::Delete)
            } else if j < m.b {
                Some(Tag::Insert)
            } else {
                None
            };
            if let Some(tag) = tag {
                out.push(Opcode::new(tag, i, m.a, j, m.b));
            }
            i = m.a + m.size;
            j = m.b + m.size;
            if m.size > 0 {
                out.push(Opcode::new(Tag::Equal, m.a, i, m.b, j));
            }
        }
        out
    }

    /// Opcodes grouped into hunks with up to `n` lines of context, like a unified diff.
    /// Identical (or empty) sequences produce no groups.
    pub fn grouped_opcodes(&self, n: usize) -> Vec<Vec<Opcode>> {
        let mut codes = self.opcodes();
        if codes.is_empty() {
            codes.push(Opcode::new(Tag::Equal, 0, 1, 0, 1));
        }
        if let Some(first) = codes.first_mut().filter(|c| c.tag == Tag::Equal) {
            first.i1 = first.i1.max(first.i2.saturating_sub(n));
            first.j1 = first.j1.max(first.j2.saturating_sub(n));
        }
        if let Some(last) = codes.last_mut().filter(|c| c.tag == Tag::Equal) {
            last.i2 = last.i2.min(last.i1 + n);
            last.j2 = last.j2.min(last.j1 + n);
        }
        let nn = n + n;
        let mut groups = Vec::new();
        let mut group: Vec<Opcode> = Vec::new();
        for mut c in codes {
            if c.tag == Tag::Equal && c.i2 - c.i1 > nn {
                group.push(Opcode::new(
                    Tag::Equal,
                    c.i1,
                    c.i2.min(c.i1 + n),
                    c.j1,
                    c.j2.min(c.j1 + n),
                ));
                groups.push(std::mem::take(&mut group));
                c.i1 = c.i1.max(c.i2.saturating_sub(n));
                c.j1 = c.j1.max(c.j2.saturating_sub(n));
            }
            group.push(c);
        }
        let lone_equal = group.len() == 1 && group[0].tag == Tag::Equal;
        if !group.is_empty() && !lone_equal {
            groups.push(group);
        }
        groups
    }

    /// `2 * matches / (len(a) + len(b))`, or 1.0 for two empty sequences.
    pub fn ratio(&self) -> f64 {
        let matches: usize = self.matching_blocks().iter().map(|m| m.size).sum();
        calculate_ratio(matches, self.a.len() + self.b.len())
    }

    /// An upper bound on `ratio()` from element counts alone.
    pub fn quick_ratio(&self) -> f64 {
        let fullbcount = self.fullbcount.get_or_init(|| {
            let mut counts: HashMap<&'a T, usize> = HashMap::new();
            for elt in self.b {
                *counts.entry(elt).or_default() += 1;
            }
            counts
        });
        let mut avail: HashMap<&T, isize> = HashMap::new();
        let mut matches = 0usize;
        for elt in self.a {
            let numb = match avail.get(elt) {
                Some(&n) => n,
                None => fullbcount.get(elt).copied().unwrap_or(0) as isize,
            };
            avail.insert(elt, numb - 1);
            if numb > 0 {
                matches += 1;
            }
        }
        calculate_ratio(matches, self.a.len() + self.b.len())
    }
}

fn calculate_ratio(matches: usize, length: usize) -> f64 {
    if length > 0 {
        2.0 * matches as f64 / length as f64
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    fn ops(m: &SequenceMatcher<'_, String>) -> Vec<(&'static str, usize, usize, usize, usize)> {
        m.opcodes()
            .into_iter()
            .map(|o| (o.tag.as_str(), o.i1, o.i2, o.j1, o.j2))
            .collect()
    }

    #[test]
    fn simple_shift() {
        let (a, b) = (words("a b c d"), words("b c d e"));
        let m = SequenceMatcher::new(&a, &b);
        assert_eq!(
            ops(&m),
            vec![
                ("delete", 0, 1, 0, 0),
                ("equal", 1, 4, 0, 3),
                ("insert", 4, 4, 3, 4)
            ]
        );
        assert!((m.ratio() - 0.75).abs() < 1e-12);
        assert!((m.quick_ratio() - 0.75).abs() < 1e-12);
    }

    #[test]
    fn insertion_in_middle() {
        let a = words("private Thread currentThread;");
        let b = words("private volatile Thread currentThread;");
        let m = SequenceMatcher::new(&a, &b);
        assert_eq!(
            ops(&m),
            vec![
                ("equal", 0, 1, 0, 1),
                ("insert", 1, 1, 1, 2),
                ("equal", 1, 3, 2, 4)
            ]
        );
        assert!((m.ratio() - 6.0 / 7.0).abs() < 1e-12);
    }

    #[test]
    fn replace_and_edges() {
        let (a, b) = (words("q a b x c d"), words("a b y c d f"));
        let m = SequenceMatcher::new(&a, &b);
        assert_eq!(
            ops(&m),
            vec![
                ("delete", 0, 1, 0, 0),
                ("equal", 1, 3, 0, 2),
                ("replace", 3, 4, 2, 3),
                ("equal", 4, 6, 3, 5),
                ("insert", 6, 6, 5, 6),
            ]
        );
    }

    #[test]
    fn blank_junk_extends_but_does_not_anchor() {
        let a: Vec<String> = ["a", "b", "", "c", "d", "e"].map(String::from).to_vec();
        let b: Vec<String> = ["a", "b", "", "x", "d", "e"].map(String::from).to_vec();
        let m = SequenceMatcher::with_junk(&a, &b, |s| s.trim().is_empty());
        assert_eq!(
            ops(&m),
            vec![
                ("equal", 0, 3, 0, 3),
                ("replace", 3, 4, 3, 4),
                ("equal", 4, 6, 4, 6)
            ]
        );
        assert!((m.ratio() - 10.0 / 12.0).abs() < 1e-12);
    }

    #[test]
    fn repeated_elements_keep_the_larger_block() {
        let mut a = vec!["x".to_string(); 10];
        a.push("y".into());
        let mut b = vec!["y".to_string()];
        b.extend(std::iter::repeat_n("x".to_string(), 10));
        let m = SequenceMatcher::new(&a, &b);
        assert_eq!(
            ops(&m),
            vec![
                ("insert", 0, 0, 0, 1),
                ("equal", 0, 10, 1, 11),
                ("delete", 10, 11, 11, 11)
            ]
        );
        assert!((m.quick_ratio() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn grouped_opcodes_on_identical_or_empty_is_empty() {
        let a = words("a b c");
        assert!(SequenceMatcher::new(&a, &a).grouped_opcodes(3).is_empty());
        let e: Vec<String> = vec![];
        assert!(SequenceMatcher::new(&e, &e).grouped_opcodes(3).is_empty());
        assert!((SequenceMatcher::new(&e, &e).ratio() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn grouped_opcodes_splits_on_long_equal_runs() {
        let a: Vec<String> = (0..20).map(|i| format!("l{i}")).collect();
        let mut b = a.clone();
        b.insert(1, "new".into());
        b[14] = "changed".into(); // b index 14 == a index 13
        let m = SequenceMatcher::with_junk(&a, &b, |s| s.is_empty());
        let groups: Vec<Vec<_>> = m
            .grouped_opcodes(3)
            .into_iter()
            .map(|g| {
                g.into_iter()
                    .map(|o| (o.tag.as_str(), o.i1, o.i2, o.j1, o.j2))
                    .collect()
            })
            .collect();
        assert_eq!(
            groups,
            vec![
                vec![
                    ("equal", 0, 1, 0, 1),
                    ("insert", 1, 1, 1, 2),
                    ("equal", 1, 4, 2, 5)
                ],
                vec![
                    ("equal", 10, 13, 11, 14),
                    ("replace", 13, 14, 14, 15),
                    ("equal", 14, 17, 15, 18)
                ],
            ]
        );
    }
}
