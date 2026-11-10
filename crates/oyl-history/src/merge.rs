//! Folding a branch back into the paper, as pure logic: how a file the
//! agent changed relates to what the paper did with it meanwhile, three-way
//! merges of text (through `diffy`), the conflicts they leave, and how a
//! [`Resolution`] settles them. Nothing here reads or writes a file.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::PathBuf;

use diffy::{ConflictStyle, IncompleteHunkStyle, MergeOptions};
use serde::{Deserialize, Serialize};

use crate::diff::{FileDiff, count_lines};
use crate::error::Result;
use crate::store::hash_text;

/// The shortest conflict markers used (git's and diffy's default).
const MIN_MARKER: usize = 7;

/// The characters conflict markers are made of.
const MARKER_CHARS: [char; 4] = ['<', '|', '=', '>'];

/// How a file the agent may have changed relates to the paper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// The agent left the file as it was.
    Untouched,
    /// Only the agent changed it: its version is taken as it is.
    AgentOnly,
    /// Both changed it, to the same content: the paper already holds the
    /// agent's result.
    Same,
    /// Both changed it, differently: it has to be merged.
    Both,
}

/// Classifies one file. `same_result` (whether the paper now holds exactly
/// what the agent made) is only asked when both sides changed the file.
pub(crate) fn classify(
    agent_changed: bool,
    main_changed: bool,
    same_result: impl FnOnce() -> Result<bool>,
) -> Result<Change> {
    Ok(if !agent_changed {
        Change::Untouched
    } else if !main_changed {
        Change::AgentOnly
    } else if same_result()? {
        Change::Same
    } else {
        Change::Both
    })
}

/// Identifies a conflict of a [`Review`].
///
/// It is derived from the conflict's file and texts, so a review recomputed
/// after the paper changed elsewhere gives an unchanged conflict the same
/// id, and a choice already made for it still applies.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConflictId(String);

impl ConflictId {
    /// The id of the conflict described by `parts`.
    fn of(parts: &[&str]) -> ConflictId {
        let mut text = String::new();
        for part in parts {
            // Length-prefixed, so that no two lists of parts collide.
            text.push_str(&part.len().to_string());
            text.push(':');
            text.push_str(part);
        }
        ConflictId(hash_text(&text)[..16].to_owned())
    }

    /// The id as text (16 lowercase hex digits).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ConflictId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which side settles a conflict.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Choice {
    /// The paper's side: what the user has.
    Main,
    /// The agent's side (the default).
    #[default]
    Agent,
    /// The paper's lines followed by the agent's. Only for
    /// [`ConflictKind::Lines`].
    Both,
}

/// How each conflict of a review is settled. A conflict without a choice of
/// its own takes the agent's side; choices for conflicts the review does not
/// have are ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    choices: BTreeMap<ConflictId, Choice>,
}

impl Resolution {
    /// Every conflict settled with the agent's side.
    pub fn new() -> Resolution {
        Resolution::default()
    }

    /// Settles conflict `id` with `choice`.
    pub fn set(&mut self, id: ConflictId, choice: Choice) -> &mut Resolution {
        self.choices.insert(id, choice);
        self
    }

    /// [`set`](Self::set), by value.
    #[must_use]
    pub fn with(mut self, id: ConflictId, choice: Choice) -> Resolution {
        self.set(id, choice);
        self
    }

    /// How conflict `id` is settled.
    pub fn choice(&self, id: &ConflictId) -> Choice {
        self.choices.get(id).copied().unwrap_or_default()
    }
}

/// A run of lines of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LineSpan {
    /// 1-based number of the first line; for an empty span, of the line it
    /// comes before.
    pub start: u32,
    /// Number of lines.
    pub count: u32,
}

/// What kind of conflict it is, which decides what the choices do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConflictKind {
    /// Both changed the same lines of a text file — or both created the
    /// file, with different text. [`Choice::Main`] keeps the paper's lines,
    /// [`Choice::Agent`] the agent's, [`Choice::Both`] the paper's then the
    /// agent's.
    Lines {
        /// Where the conflict is in the branch's copy of the file, which
        /// holds the merge with the agent's side of every conflict: the
        /// agent's lines (an empty span where the agent removed them).
        lines: LineSpan,
    },
    /// Both changed — or both created — a file that is not merged line by
    /// line: an image, a PDF, anything that is not UTF-8 text or is larger
    /// than [`MAX_FILE_BYTES`](crate::MAX_FILE_BYTES). Main or Agent keeps
    /// that side's whole file.
    File,
    /// The agent deleted a file the paper changed meanwhile. Main keeps the
    /// paper's file; Agent deletes it.
    DeletedByAgent,
    /// The paper deleted a file the agent changed. Main leaves it deleted;
    /// Agent brings back the agent's version.
    DeletedInMain,
}

/// A place where the paper and the agent changed the same thing
/// differently.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    /// Stable identity, for [`Resolution::set`].
    pub id: ConflictId,
    /// The file, relative to the paper folder.
    pub file: PathBuf,
    /// What kind of conflict it is.
    pub kind: ConflictKind,
    /// The paper's side ("ours"): its version of the lines, or for the
    /// other kinds its whole file when that is text (else empty).
    pub main: String,
    /// The agent's side ("theirs"), the same way.
    pub agent: String,
    /// What both started from, the same way (empty when the file did not
    /// exist).
    pub base: String,
}

/// One file as it would change, compared line by line when it is text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    /// The path (relative to the paper folder), whether the file is added,
    /// modified or deleted, and — for text — the line counts and hunks.
    pub diff: FileDiff,
    /// The file was compared as a whole, not line by line: one side is not
    /// UTF-8 text or is larger than [`MAX_FILE_BYTES`](crate::MAX_FILE_BYTES)
    /// (an image, a PDF, …). Its diff has no hunks and counts no lines.
    pub binary: bool,
}

/// What applying a branch would do to the paper, and the conflicts to
/// settle first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    /// Every file applying would change in the paper, from the paper as it
    /// is to the branch's merged result with the agent's side of every
    /// conflict, in path order.
    pub files: Vec<ChangedFile>,
    /// The conflicts, in path order and, within a file, in line order.
    pub conflicts: Vec<Conflict>,
}

impl Review {
    /// Whether any conflict needs a choice (the default is the agent's
    /// side).
    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }
}

/// One stretch of a merged text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Piece {
    /// Text both sides agree on after the merge.
    Clean(String),
    /// Lines both sides changed differently.
    Conflict {
        main: String,
        base: String,
        agent: String,
    },
}

/// A three-way merge of one text file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Merged {
    pieces: Vec<Piece>,
}

impl Merged {
    /// The conflicts, in order: (main, base, agent).
    pub(crate) fn conflicts(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.pieces.iter().filter_map(|piece| match piece {
            Piece::Conflict { main, base, agent } => Some((main.as_str(), base.as_str(), agent.as_str())),
            Piece::Clean(_) => None,
        })
    }

    /// The merged text with conflict `i` settled by `choose(i)`.
    /// [`Choice::Both`] puts the paper's lines first, on a line of their own.
    pub(crate) fn render(&self, mut choose: impl FnMut(usize) -> Choice) -> String {
        let mut out = String::new();
        let mut index = 0;
        for piece in &self.pieces {
            match piece {
                Piece::Clean(text) => out.push_str(text),
                Piece::Conflict { main, agent, .. } => {
                    match choose(index) {
                        Choice::Main => out.push_str(main),
                        Choice::Agent => out.push_str(agent),
                        Choice::Both => {
                            out.push_str(main);
                            if !main.is_empty() && !agent.is_empty() && !main.ends_with('\n') {
                                out.push('\n');
                            }
                            out.push_str(agent);
                        }
                    }
                    index += 1;
                }
            }
        }
        out
    }

    /// Where each conflict's agent side is in the text rendered with the
    /// agent's side everywhere.
    pub(crate) fn agent_spans(&self) -> Vec<LineSpan> {
        let mut line: u32 = 1;
        let mut spans = Vec::new();
        for piece in &self.pieces {
            let text = match piece {
                Piece::Clean(text) => text,
                Piece::Conflict { agent, .. } => {
                    spans.push(LineSpan {
                        start: line,
                        count: count_lines(agent),
                    });
                    agent
                }
            };
            let breaks = text.bytes().filter(|&b| b == b'\n').count();
            line = line.saturating_add(u32::try_from(breaks).unwrap_or(u32::MAX));
        }
        spans
    }
}

/// Merges the changes from `base` to `main` and from `base` to `agent`,
/// line by line.
pub(crate) fn merge3(base: &str, main: &str, agent: &str) -> Merged {
    let marker = marker_len(&[base, main, agent]);
    let mut options = MergeOptions::new();
    options
        .set_conflict_style(ConflictStyle::Diff3)
        .set_conflict_marker_length(marker)
        // Markers right after an unterminated last line, so that no line
        // ending is ever invented.
        .set_incomplete_hunk_style(IncompleteHunkStyle::Diff3);
    let pieces = match options.merge(base, main, agent) {
        Ok(text) => clean(text),
        Err(marked) => match parse_markers(&marked, marker) {
            Some(pieces) => tighten(pieces),
            None => {
                log::error!("history: could not read a merge's conflict markers; the file conflicts as a whole");
                vec![Piece::Conflict {
                    main: main.to_owned(),
                    base: base.to_owned(),
                    agent: agent.to_owned(),
                }]
            }
        },
    };
    Merged { pieces }
}

/// Moves the lines both sides of a conflict share at its start and at its
/// end out of it, into clean text, and joins clean neighbours. The base
/// stays whole: it is what both started from around the conflict.
fn tighten(pieces: Vec<Piece>) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    let push_clean = |out: &mut Vec<Piece>, text: &str| {
        if text.is_empty() {
            return;
        }
        if let Some(Piece::Clean(last)) = out.last_mut() {
            last.push_str(text);
        } else {
            out.push(Piece::Clean(text.to_owned()));
        }
    };
    for piece in pieces {
        match piece {
            Piece::Clean(text) => push_clean(&mut out, &text),
            Piece::Conflict { main, base, agent } => {
                let main_lines: Vec<&str> = main.split_inclusive('\n').collect();
                let agent_lines: Vec<&str> = agent.split_inclusive('\n').collect();
                let prefix = main_lines
                    .iter()
                    .zip(&agent_lines)
                    .take_while(|(m, a)| m == a)
                    .count();
                let room = main_lines.len().min(agent_lines.len()) - prefix;
                let suffix = main_lines
                    .iter()
                    .rev()
                    .zip(agent_lines.iter().rev())
                    .take(room)
                    .take_while(|(m, a)| m == a)
                    .count();
                push_clean(&mut out, &main_lines[..prefix].concat());
                let main_rest = main_lines[prefix..main_lines.len() - suffix].concat();
                let agent_rest = agent_lines[prefix..agent_lines.len() - suffix].concat();
                // Sides that turn out equal are no conflict at all.
                if main_rest == agent_rest {
                    push_clean(&mut out, &main_rest);
                } else {
                    out.push(Piece::Conflict {
                        main: main_rest,
                        base,
                        agent: agent_rest,
                    });
                }
                push_clean(&mut out, &main_lines[main_lines.len() - suffix..].concat());
            }
        }
    }
    out
}

fn clean(text: String) -> Vec<Piece> {
    if text.is_empty() { Vec::new() } else { vec![Piece::Clean(text)] }
}

/// A marker length that cannot occur in any of `texts`: longer than the
/// longest run of any marker character in them. Then every marker found in
/// the merge output is one the merge wrote.
fn marker_len(texts: &[&str]) -> usize {
    let mut longest = 0;
    for text in texts {
        let mut run = 0;
        let mut previous = None;
        for c in text.chars() {
            if MARKER_CHARS.contains(&c) {
                run = if previous == Some(c) { run + 1 } else { 1 };
                longest = longest.max(run);
                previous = Some(c);
            } else {
                run = 0;
                previous = None;
            }
        }
    }
    (longest + 1).max(MIN_MARKER)
}

/// Splits diffy's Diff3-style output (markers `marker` characters long)
/// into clean text and conflicts. `None` if it is not shaped as expected.
pub(crate) fn parse_markers(output: &str, marker: usize) -> Option<Vec<Piece>> {
    let start = format!("{} ours\n", "<".repeat(marker));
    let original = format!("{} original\n", "|".repeat(marker));
    let separator = format!("{}\n", "=".repeat(marker));
    let end = format!("{} theirs\n", ">".repeat(marker));
    let mut pieces = Vec::new();
    let mut rest = output;
    while let Some(at) = rest.find(&start) {
        if at > 0 {
            pieces.push(Piece::Clean(rest[..at].to_owned()));
        }
        let after = &rest[at + start.len()..];
        let m = after.find(&original)?;
        let main = &after[..m];
        let after = &after[m + original.len()..];
        let s = after.find(&separator)?;
        let base = &after[..s];
        let after = &after[s + separator.len()..];
        let e = after.find(&end)?;
        let agent = &after[..e];
        pieces.push(Piece::Conflict {
            main: main.to_owned(),
            base: base.to_owned(),
            agent: agent.to_owned(),
        });
        rest = &after[e + end.len()..];
    }
    if rest.contains(&original) || rest.contains(&separator) || rest.contains(&end) {
        return None;
    }
    if !rest.is_empty() {
        pieces.push(Piece::Clean(rest.to_owned()));
    }
    Some(pieces)
}

/// How the paper and the agent's versions of a file both sides changed
/// come together.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FileMerge {
    /// Text merged line by line; conflicts, if any, are within it.
    Lines(Merged),
    /// One conflict about the whole file.
    Whole(WholeKind),
}

/// A conflict about a whole file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WholeKind {
    File,
    DeletedByAgent,
    DeletedInMain,
}

impl WholeKind {
    pub(crate) fn kind(self) -> ConflictKind {
        match self {
            WholeKind::File => ConflictKind::File,
            WholeKind::DeletedByAgent => ConflictKind::DeletedByAgent,
            WholeKind::DeletedInMain => ConflictKind::DeletedInMain,
        }
    }

    fn tag(self) -> &'static str {
        match self {
            WholeKind::File => "file",
            WholeKind::DeletedByAgent => "deleted-by-agent",
            WholeKind::DeletedInMain => "deleted-in-main",
        }
    }
}

/// The ids of the conflicts of a file merge, in order. `main_hash` and
/// `agent_hash` (content hashes, `None` when absent) identify a whole-file
/// conflict.
pub(crate) fn conflict_ids(
    key: &str,
    merge: &FileMerge,
    main_hash: Option<&str>,
    agent_hash: Option<&str>,
) -> Vec<ConflictId> {
    match merge {
        FileMerge::Lines(merged) => {
            let mut seen: HashMap<(&str, &str, &str), usize> = HashMap::new();
            merged
                .conflicts()
                .map(|texts| {
                    let n = seen.entry(texts).or_default();
                    let occurrence = n.to_string();
                    *n += 1;
                    let (main, base, agent) = texts;
                    ConflictId::of(&[key, "lines", &occurrence, main, base, agent])
                })
                .collect()
        }
        FileMerge::Whole(kind) => vec![ConflictId::of(&[
            key,
            kind.tag(),
            main_hash.unwrap_or(""),
            agent_hash.unwrap_or(""),
        ])],
    }
}

/// What to do with one file in the paper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Settled {
    /// Leave the paper's file (or its absence) as it is.
    KeepMain,
    /// Put the agent's file in place.
    TakeAgent,
    /// Delete the paper's file.
    Delete,
    /// Write this text.
    Text(String),
}

/// Settles a file merge whose conflicts have ids `ids` by `resolution`.
/// `Err` names a conflict that was asked to keep both sides but cannot.
pub(crate) fn settle(
    merge: &FileMerge,
    ids: &[ConflictId],
    resolution: &Resolution,
) -> std::result::Result<Settled, ConflictId> {
    let choice_of = |i: usize| ids.get(i).map_or(Choice::Agent, |id| resolution.choice(id));
    match merge {
        FileMerge::Lines(merged) => Ok(Settled::Text(merged.render(choice_of))),
        FileMerge::Whole(kind) => match (kind, choice_of(0)) {
            (_, Choice::Both) => Err(ids.first().cloned().unwrap_or_else(|| ConflictId::of(&[]))),
            (_, Choice::Main) => Ok(Settled::KeepMain),
            (WholeKind::File | WholeKind::DeletedInMain, Choice::Agent) => Ok(Settled::TakeAgent),
            (WholeKind::DeletedByAgent, Choice::Agent) => Ok(Settled::Delete),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conflict(main: &str, base: &str, agent: &str) -> Piece {
        Piece::Conflict {
            main: main.to_owned(),
            base: base.to_owned(),
            agent: agent.to_owned(),
        }
    }

    fn clean_piece(text: &str) -> Piece {
        Piece::Clean(text.to_owned())
    }

    const BASE: &str = "Title\n\nFirst paragraph.\n\nSecond paragraph.\n\nThird paragraph.\n";

    #[test]
    fn classification() {
        let same = || Ok(true);
        let differ = || Ok(false);
        assert_eq!(classify(false, true, differ).unwrap(), Change::Untouched);
        assert_eq!(classify(true, false, differ).unwrap(), Change::AgentOnly);
        assert_eq!(classify(true, true, same).unwrap(), Change::Same);
        assert_eq!(classify(true, true, differ).unwrap(), Change::Both);
        // The comparison is only made when both changed the file.
        let never = || -> Result<bool> { panic!("not asked") };
        assert_eq!(classify(true, false, never).unwrap(), Change::AgentOnly);
    }

    #[test]
    fn changes_to_different_lines_merge_cleanly() {
        let main = BASE.replace("First paragraph.", "First paragraph, by the user.");
        let agent = BASE.replace("Third paragraph.", "Third paragraph, by the agent.");
        let merged = merge3(BASE, &main, &agent);
        assert_eq!(merged.conflicts().count(), 0);
        assert_eq!(
            merged.render(|_| Choice::Agent),
            BASE.replace("First paragraph.", "First paragraph, by the user.")
                .replace("Third paragraph.", "Third paragraph, by the agent.")
        );
        assert!(merged.agent_spans().is_empty());
    }

    #[test]
    fn changes_to_the_same_lines_conflict() {
        let main = BASE.replace("Second paragraph.", "Second paragraph (user).");
        let agent = BASE
            .replace("Second paragraph.", "Second paragraph (agent).")
            .replace("Title", "A Better Title");
        let merged = merge3(BASE, &main, &agent);
        let conflicts: Vec<_> = merged.conflicts().collect();
        assert_eq!(
            conflicts,
            [("Second paragraph (user).\n", "Second paragraph.\n", "Second paragraph (agent).\n")]
        );
        assert_eq!(merged.agent_spans(), [LineSpan { start: 5, count: 1 }]);
        let pick = |choice| merged.render(move |_| choice);
        assert_eq!(pick(Choice::Agent), agent);
        assert_eq!(
            pick(Choice::Main),
            BASE.replace("Second paragraph.", "Second paragraph (user).").replace("Title", "A Better Title")
        );
        assert_eq!(
            pick(Choice::Both),
            BASE.replace("Second paragraph.", "Second paragraph (user).\nSecond paragraph (agent).")
                .replace("Title", "A Better Title")
        );
    }

    #[test]
    fn marker_like_text_is_never_mistaken_for_a_marker() {
        let base = "<<<<<<< ours\nx\n=======\n|||||||| original\n>>>>>>> theirs\n";
        let main = "<<<<<<< ours\nmain\n=======\n|||||||| original\n>>>>>>> theirs\n";
        let agent = "<<<<<<< ours\nagent\n=======\n|||||||| original\n>>>>>>> theirs\n";
        assert!(marker_len(&[base, main, agent]) > 8);
        let merged = merge3(base, main, agent);
        assert_eq!(merged.conflicts().collect::<Vec<_>>(), [("main\n", "x\n", "agent\n")]);
        assert_eq!(merged.render(|_| Choice::Agent), agent);
        assert_eq!(merged.render(|_| Choice::Main), main);
    }

    #[test]
    fn unterminated_last_lines_keep_their_endings() {
        let merged = merge3("a\nend", "a\nend by user", "a\nend by agent");
        assert_eq!(merged.conflicts().collect::<Vec<_>>(), [("end by user", "end", "end by agent")]);
        assert_eq!(merged.render(|_| Choice::Agent), "a\nend by agent");
        assert_eq!(merged.render(|_| Choice::Main), "a\nend by user");
        // Both: the paper's line gets a line ending, the agent's keeps none.
        assert_eq!(merged.render(|_| Choice::Both), "a\nend by user\nend by agent");
        assert_eq!(merged.agent_spans(), [LineSpan { start: 2, count: 1 }]);

        // Marker characters right before a marker the merge wrote.
        let merged = merge3("x\n<<", "x\n<<a", "x\n<<b");
        assert_eq!(merged.conflicts().collect::<Vec<_>>(), [("<<a", "<<", "<<b")]);
        let merged = merge3("x\n==", "x\n==a", "x\n==b");
        assert_eq!(merged.conflicts().collect::<Vec<_>>(), [("==a", "==", "==b")]);
    }

    #[test]
    fn both_created_conflicts_over_everything() {
        let merged = merge3("", "one\ntwo\n", "uno\ndos\n");
        assert_eq!(merged.conflicts().collect::<Vec<_>>(), [("one\ntwo\n", "", "uno\ndos\n")]);
        assert_eq!(merged.render(|_| Choice::Both), "one\ntwo\nuno\ndos\n");
        assert_eq!(merged.agent_spans(), [LineSpan { start: 1, count: 2 }]);
    }

    #[test]
    fn deleted_lines_have_empty_spans() {
        let base = "a\nb\nc\nd\n";
        let merged = merge3(base, "a\nB\nc\nd\n", "a\nc\nD\n");
        let conflicts: Vec<_> = merged.conflicts().collect();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0], ("B\n", "b\n", ""));
        assert_eq!(merged.agent_spans(), [LineSpan { start: 2, count: 0 }]);
        assert_eq!(merged.render(|_| Choice::Agent), "a\nc\nD\n");
        assert_eq!(merged.render(|_| Choice::Main), "a\nB\nc\nD\n");
    }

    #[test]
    fn several_conflicts_are_settled_one_by_one() {
        let base = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        let main = "1\nM2\n3\n4\n5\n6\n7\nM8\n9\n";
        let agent = "1\nA2\n3\n4\n5\nnew\n6\n7\nA8\n9\n";
        let merged = merge3(base, main, agent);
        assert_eq!(merged.conflicts().count(), 2);
        assert_eq!(
            merged.agent_spans(),
            [LineSpan { start: 2, count: 1 }, LineSpan { start: 9, count: 1 }]
        );
        let text = merged.render(|i| if i == 0 { Choice::Main } else { Choice::Agent });
        assert_eq!(text, "1\nM2\n3\n4\n5\nnew\n6\n7\nA8\n9\n");
    }

    #[test]
    fn lines_both_sides_share_leave_the_conflict() {
        // Both changed the first line the same way; only the paper changed
        // the second. diffy sees one conflict over both lines; the shared
        // first line is moved out of it.
        let merged = merge3("a\nb\nc\n", "A\nB\nc\n", "A\nb\nc\n");
        assert_eq!(merged.conflicts().collect::<Vec<_>>(), [("B\n", "a\nb\n", "b\n")]);
        assert_eq!(merged.agent_spans(), [LineSpan { start: 2, count: 1 }]);
        assert_eq!(merged.render(|_| Choice::Main), "A\nB\nc\n");
        assert_eq!(merged.render(|_| Choice::Agent), "A\nb\nc\n");
        assert_eq!(merged.render(|_| Choice::Both), "A\nB\nb\nc\n");

        // Shared last lines too, and a side left empty.
        let merged = merge3("x\ny\n", "X\nend\n", "X\nmore\nend\n");
        assert_eq!(merged.conflicts().collect::<Vec<_>>(), [("", "x\ny\n", "more\n")]);
        assert_eq!(merged.render(|_| Choice::Main), "X\nend\n");
        assert_eq!(merged.render(|_| Choice::Agent), "X\nmore\nend\n");
        assert_eq!(
            tighten(vec![clean_piece("a\n"), conflict("s\nm\n", "b\n", "s\nt\n"), clean_piece("z\n")]),
            [clean_piece("a\ns\n"), conflict("m\n", "b\n", "t\n"), clean_piece("z\n")]
        );
    }

    #[test]
    fn parsing_rejects_damaged_output() {
        assert_eq!(parse_markers("plain\n", 7), Some(vec![clean_piece("plain\n")]));
        assert_eq!(parse_markers("", 7), Some(vec![]));
        assert_eq!(parse_markers("<<<<<<< ours\nx\n", 7), None);
        assert_eq!(parse_markers("x\n=======\n", 7), None);
        assert_eq!(
            parse_markers("a\n<<<<<<< ours\nm\n||||||| original\nb\n=======\nt\n>>>>>>> theirs\nz\n", 7),
            Some(vec![clean_piece("a\n"), conflict("m\n", "b\n", "t\n"), clean_piece("z\n")])
        );
    }

    #[test]
    fn ids_are_stable_and_distinct() {
        let merged = merge3("a\nx\nb\nx\nc\n", "a\nm\nb\nm\nc\n", "a\nt\nb\nt\nc\n");
        let merge = FileMerge::Lines(merged.clone());
        let ids = conflict_ids("main.typ", &merge, None, None);
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1], "identical conflicts are told apart by position");
        assert_eq!(ids, conflict_ids("main.typ", &FileMerge::Lines(merged), None, None));
        assert_ne!(ids, conflict_ids("other.typ", &merge, None, None));
        assert_eq!(ids[0].as_str().len(), 16);

        let whole = FileMerge::Whole(WholeKind::File);
        let a = conflict_ids("fig.png", &whole, Some("m1"), Some("a1"));
        assert_eq!(a, conflict_ids("fig.png", &whole, Some("m1"), Some("a1")));
        assert_ne!(a, conflict_ids("fig.png", &whole, Some("m2"), Some("a1")));
    }

    #[test]
    fn settling() {
        let merged = merge3("x\n", "m\n", "a\n");
        let merge = FileMerge::Lines(merged);
        let ids = conflict_ids("f.typ", &merge, None, None);
        let id = ids[0].clone();
        assert_eq!(settle(&merge, &ids, &Resolution::new()), Ok(Settled::Text("a\n".into())));
        let main = Resolution::new().with(id.clone(), Choice::Main);
        assert_eq!(settle(&merge, &ids, &main), Ok(Settled::Text("m\n".into())));
        let both = Resolution::new().with(id, Choice::Both);
        assert_eq!(settle(&merge, &ids, &both), Ok(Settled::Text("m\na\n".into())));

        for (kind, agent) in [
            (WholeKind::File, Settled::TakeAgent),
            (WholeKind::DeletedInMain, Settled::TakeAgent),
            (WholeKind::DeletedByAgent, Settled::Delete),
        ] {
            let merge = FileMerge::Whole(kind);
            let ids = conflict_ids("f.png", &merge, Some("m"), Some("a"));
            let id = ids[0].clone();
            assert_eq!(settle(&merge, &ids, &Resolution::new()), Ok(agent));
            let main = Resolution::new().with(id.clone(), Choice::Main);
            assert_eq!(settle(&merge, &ids, &main), Ok(Settled::KeepMain));
            let both = Resolution::new().with(id.clone(), Choice::Both);
            assert_eq!(settle(&merge, &ids, &both), Err(id));
        }
    }

    #[test]
    fn resolutions_default_to_the_agent() {
        let id = ConflictId::of(&["x"]);
        let mut resolution = Resolution::new();
        assert_eq!(resolution.choice(&id), Choice::Agent);
        resolution.set(id.clone(), Choice::Both);
        assert_eq!(resolution.choice(&id), Choice::Both);
        let json = serde_json::to_string(&resolution).unwrap();
        assert_eq!(serde_json::from_str::<Resolution>(&json).unwrap(), resolution);
    }
}
