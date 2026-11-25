//! Where each writer's audit chain stands in a file the file sink appends
//! to (ADR-0070 clause 5): the number and the digest of its last record,
//! which the next record it appends carries as the one before it.
//!
//! Every program of a writer may append to one file, one process after
//! another and two at once, so the sink appends holding [`LOCK_NAME`]
//! beside the file, and what this process knows of the file is brought up
//! to date under it: what was appended since it last looked is read, never
//! the whole history. A writer this process has not met yet is found by
//! reading back from the end of the file to its last record: the last
//! record of its chain, wherever the writer restarted, found by the line
//! that names its writer and the one table holding it parsed, so what is
//! passed over is read and searched once and never parsed. A file that got
//! shorter was replaced, and is looked at again. A record whose write was
//! interrupted is no record: sealed apart, it is in no chain, and the next
//! record of its writer goes on from the last whole one.
//!
//! The file is opened to read only where there is something to read: what
//! another process appended since, or a writer not met yet. Opening a file
//! to read that was written since its last open is what an operating
//! system's file scanner waits on — 37 ms a record on Windows on
//! 2026-11-13, where the whole durable write was 1.6 ms (ADR-0062,
//! amendment 2026-11-13) — so a file this process left whole at the length
//! it has now is never read again.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use memchr::memmem::{self, Finder, FinderRev};
use xcore::audit_chain::{FIRST, Head};
use xcore::lock::Locked;

use audit::audit_entry::{AuditEntry, parse};
use audit::audit_record::writer_line;

/// The file beside `audit.toml` an appender holds while it appends: the
/// operating system's own lock, which ends with the process.
pub const LOCK_NAME: &str = "audit.lock";

/// How much of the file's end is read first to find a writer's last
/// record; four times more each time it is not there.
const WINDOW: u64 = 64 * 1024;

/// A table's first line, with the end of the line before it.
const HEADER: &[u8] = b"\n[[record]]\n";

/// What this process knows of one file: how far it has looked, and the
/// head of each writer it has met.
#[derive(Default)]
struct Tail {
    /// Whether it has looked at all.
    looked: bool,
    length: u64,
    heads: HashMap<String, Head>,
    /// Whether this process's own append ended the file at `length`, whole.
    whole: bool,
}

fn tails() -> &'static Mutex<HashMap<PathBuf, Tail>> {
    static TAILS: LazyLock<Mutex<HashMap<PathBuf, Tail>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    &TAILS
}

/// Whether this process left the file at `path` whole at `length`, the
/// length it has now: its own append ended it there, and nothing changed
/// its length since — so its end needs no seal, and is not read.
pub(crate) fn whole_at(path: &Path, length: u64) -> bool {
    tails()
        .locked()
        .get(path)
        .is_some_and(|tail| tail.looked && tail.whole && tail.length == length)
}

/// The head of `writer`'s chain in the file at `path`, `length` long now,
/// read while the caller holds the lock, through a handle of its own that
/// only reads and is opened only where there is something to read: the
/// file is open for writing only for the write.
pub(crate) fn head(path: &Path, length: u64, writer: &str) -> io::Result<Head> {
    let mut all = tails().locked();
    let tail = all.entry(path.to_path_buf()).or_default();
    if length < tail.length {
        *tail = Tail::default();
    }
    let mut file = None;
    if tail.looked && length > tail.length {
        let appended = read_from(file.insert(File::open(path)?), tail.length, length)?;
        for entry in parse(&appended).entries {
            if let Some(writer) = entry.writer.clone() {
                tail.heads.insert(writer, head_of(&entry));
            }
        }
        tail.whole = false;
    }
    tail.length = length;
    tail.looked = true;
    if let Some(head) = tail.heads.get(writer) {
        return Ok(*head);
    }
    let head = match (length, file) {
        (0, _) => Head::GENESIS,
        (_, Some(mut file)) => from_the_end(&mut file, length, writer)?,
        (_, None) => from_the_end(&mut File::open(path)?, length, writer)?,
    };
    tail.heads.insert(writer.to_string(), head);
    Ok(head)
}

/// `writer` appended its record `head` to `path`, which is now `length`
/// long and ends whole.
pub(crate) fn appended(path: &Path, writer: &str, head: Head, length: u64) {
    let mut all = tails().locked();
    let tail = all.entry(path.to_path_buf()).or_default();
    tail.length = length;
    tail.looked = true;
    tail.whole = true;
    tail.heads.insert(writer.to_string(), head);
}

fn head_of(entry: &AuditEntry) -> Head {
    Head {
        position: entry.position.unwrap_or_default(),
        digest: entry.digest.unwrap_or(FIRST),
    }
}

/// The bytes from `start` to `end`.
fn read_from(file: &mut File, start: u64, end: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    read_into(file, start, end, &mut bytes)?;
    Ok(bytes)
}

/// The bytes from `start` to `end` after what `bytes` holds.
fn read_into(file: &mut File, start: u64, end: u64, bytes: &mut Vec<u8>) -> io::Result<()> {
    file.seek(SeekFrom::Start(start))?;
    file.take(end - start).read_to_end(bytes)?;
    Ok(())
}

/// `writer`'s last record, read back from the end of the file a window at
/// a time, each window before the last, until the line naming the writer
/// is found in a whole table: each byte read once and searched once, and
/// only that table parsed. Until 2026-11-25 every window was parsed whole,
/// and a writer last met megabytes back took seconds over its first record
/// in a process: a System Tests far end, 1.5 to 3 s in a 7 MB file.
fn from_the_end(file: &mut File, length: u64, writer: &str) -> io::Result<Head> {
    let line = format!("\n{}", writer_line(writer));
    // What is read, from `start` to the end of the file, and how many of
    // its bytes, from its first, still begin a line to search.
    let (mut start, mut read, mut unsearched) = (length, Vec::new(), 0);
    let mut window = WINDOW;
    loop {
        let from = start.saturating_sub(window);
        let added = usize::try_from(start - from).unwrap_or(usize::MAX);
        let mut earlier = Vec::with_capacity(added.saturating_add(read.len()));
        read_into(file, from, start, &mut earlier)?;
        unsearched += earlier.len();
        earlier.extend_from_slice(&read);
        (start, read) = (from, earlier);
        match searched(&read, unsearched, line.as_bytes(), writer, start == 0) {
            Reading::Found(head) => return Ok(head),
            Reading::Further(_) if start == 0 => return Ok(Head::GENESIS),
            Reading::Further(left) => unsearched = left,
        }
        window = window.saturating_mul(4);
    }
}

/// What a window read back said of a writer's last record.
enum Reading {
    /// Its head.
    Found(Head),
    /// Not in it: how many of its first bytes still begin a line to search
    /// once the window before it is read, a table begun before it among them.
    Further(usize),
}

/// `writer`'s last whole record in `read`, among the lines `line` that
/// begin in its first `unsearched` bytes, the last first. `whole` says
/// `read` begins with the file, so a table with no header before it is
/// the file's first.
fn searched(read: &[u8], unsearched: usize, line: &[u8], writer: &str, whole: bool) -> Reading {
    let end = (unsearched + line.len() - 1).min(read.len());
    let headed = FinderRev::new(HEADER);
    for at in named(&read[..end], line).into_iter().rev() {
        let begins = headed.rfind(&read[..at]).map(|header| header + 1);
        let Some(begins) = begins.or(whole.then_some(0)) else {
            return Reading::Further(at + 1);
        };
        let ends = memmem::find(&read[at..], HEADER).map_or(read.len(), |header| at + header + 1);
        // A sealed fragment names its writer too, and is no record.
        let found = parse(&read[begins..ends])
            .entries
            .iter()
            .find(|entry| entry.writer.as_deref() == Some(writer))
            .map(head_of);
        if let Some(head) = found {
            return Reading::Found(head);
        }
    }
    Reading::Further(0)
}

/// Where `line` begins in `bytes`, each place, first to last: searched
/// forward, which memchr does a vector of bytes at a time and its search
/// back does not, so what is passed over is searched at the speed of
/// reading it.
fn named(bytes: &[u8], line: &[u8]) -> Vec<usize> {
    let finder = Finder::new(line);
    let (mut places, mut from) = (Vec::new(), 0);
    while let Some(at) = finder.find(&bytes[from..]) {
        places.push(from + at);
        from += at + 1;
    }
    places
}

/// Forget what this process knows of `path`, as a process started anew
/// would not know it.
#[cfg(test)]
pub(crate) fn forget(path: &Path) {
    tails().locked().remove(path);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use file::temporary::Folder;
    use xcore::{ExecutionPhase, Severity};

    use super::*;
    use crate::FileSink;
    use audit::audit_chain::{Break, Verdict, chains};
    use audit::audit_record::AuditRecord;
    use audit::program_audit::ProgramAudit;

    fn scratch(name: &str) -> Folder {
        Folder::new(&format!("audit-chain-{name}"))
    }

    fn act(audit: &ProgramAudit, action: &str) {
        let said = BTreeMap::from([("said".to_string(), "a \"quoted\"\nvalue".to_string())]);
        audit
            .record(
                action,
                ExecutionPhase::Begin,
                Severity::Information,
                None,
                said,
            )
            .expect("recorded");
    }

    /// The file's tables, each as written.
    fn tables(path: &Path) -> Vec<String> {
        let text = fs::read_to_string(path).expect("read");
        text.split_inclusive("\n\n").map(str::to_string).collect()
    }

    fn verdicts(path: &Path, writers: &[&str]) -> Vec<Verdict> {
        let text = fs::read_to_string(path).expect("read");
        chains(&parse(text.as_bytes()).entries, writers)
    }

    /// Four records of the program `probe`, in `directory`.
    fn four(directory: &Path) -> (ProgramAudit, PathBuf) {
        let audit = ProgramAudit::new("probe", Some(FileSink::new(directory)));
        for action in ["one", "two", "three", "four"] {
            act(&audit, action);
        }
        let path = FileSink::new(directory).path();
        (audit, path)
    }

    #[test]
    fn every_record_carries_its_place_and_a_whole_chain_verifies() {
        let directory = scratch("whole");
        let (_, path) = four(&directory);
        let text = fs::read_to_string(&path).expect("read");
        assert_eq!(text.matches("\nwriter = \"probe\"\n").count(), 4, "{text}");
        assert!(text.contains("position = \"4\"\n"), "{text}");
        assert!(text.contains(&format!("previous = \"{}\"", "0".repeat(64))));

        let verdict = &verdicts(&path, &["probe"])[0];
        assert!(verdict.whole(), "{}", verdict.said());
        assert_eq!(verdict.records, 4);
    }

    #[test]
    fn a_deleted_a_changed_and_a_reordered_record_are_each_found_where_they_are() {
        let directory = scratch("broken");
        let (_, path) = four(&directory);
        let written = tables(&path);
        let id = |table: &str| parse(table.as_bytes()).entries[0].audit_id.clone();

        let deleted = [&written[0], &written[1], &written[3]];
        fs::write(&path, deleted.map(String::as_str).concat()).expect("written");
        let broken = verdicts(&path, &["probe"]).remove(0).broken;
        let from = 3;
        let gone = Break::Deleted {
            record: id(&written[3]),
            position: 4,
            from,
            to: from,
        };
        assert_eq!(broken, Some(gone));

        let changed = written[1].replace("action = \"two\"", "action = \"zwei\"");
        fs::write(
            &path,
            [&written[0], &changed, &written[2]]
                .map(String::as_str)
                .concat(),
        )
        .expect("written");
        let broken = verdicts(&path, &["probe"]).remove(0).broken;
        assert_eq!(
            broken,
            Some(Break::Changed {
                record: id(&written[1]),
                position: 2
            })
        );

        let reordered = [&written[0], &written[2], &written[1], &written[3]];
        fs::write(&path, reordered.map(String::as_str).concat()).expect("written");
        let verdict = verdicts(&path, &["probe"]).remove(0);
        let moved = Break::OutOfOrder {
            record: id(&written[1]),
            position: 2,
            after: 3,
        };
        assert_eq!(verdict.broken, Some(moved));
        assert!(verdict.said().starts_with("FAILED: "), "{}", verdict.said());
    }

    #[test]
    fn a_writer_started_again_continues_its_chain_from_its_last_record() {
        let directory = scratch("restart");
        let (_, path) = four(&directory);
        let other = ProgramAudit::new("another", Some(FileSink::new(&*directory)));
        act(&other, "between");

        forget(&path);
        let again = ProgramAudit::new("probe", Some(FileSink::new(&*directory)));
        act(&again, "five");

        let text = fs::read_to_string(&path).expect("read");
        assert!(text.contains("position = \"5\"\n"), "{text}");
        let verdict = &verdicts(&path, &["probe"])[0];
        assert!(verdict.whole(), "{}", verdict.said());
        assert_eq!(verdict.records, 5);
    }

    /// Records of the program `another` appended to the file at `path` until
    /// it is `length` bytes long, the last one's message making up the rest.
    fn filled(path: &Path, length: usize) {
        let another = |position: u64, previous: &xcore::audit_chain::Digest, pad: usize| {
            AuditRecord {
                audit_id: xcore::AuditId::new(u128::from(position)),
                origin: audit::origin::Origin::here("another"),
                scope: None,
                action: "between".to_string(),
                phase: ExecutionPhase::Begin,
                severity: Severity::Information,
                timestamp_unix_nanos: 0,
                message: Some("x".repeat(pad)),
                properties: BTreeMap::new(),
            }
            .chained("another", position, previous)
        };
        let now = usize::try_from(fs::metadata(path).expect("there").len()).expect("small");
        let (mut text, mut previous, mut position) = (String::new(), [0; 32], 1);
        loop {
            let (table, own) = another(position, &previous, 0);
            if now + text.len() + 2 * table.len() > length {
                break;
            }
            text.push_str(&table);
            (previous, position) = (own, position + 1);
        }
        let bare = another(position, &previous, 0).0.len();
        text.push_str(&another(position, &previous, length - now - text.len() - bare).0);
        assert_eq!(now + text.len(), length);
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(path)
            .expect("opened");
        std::io::Write::write_all(&mut file, text.as_bytes()).expect("written");
    }

    /// A writer last met windows back goes on from its last record wherever
    /// a window's edge falls: before the table naming it, in the line naming
    /// it, or after that line.
    #[test]
    fn a_writer_met_windows_back_goes_on_from_its_last_record_wherever_a_window_ends() {
        let window = usize::try_from(WINDOW).expect("small");
        for (name, edge) in [
            ("header", -20),
            ("line", 3),
            ("after", 40),
            ("far", 900_000),
        ] {
            let directory = scratch(&format!("window-{name}"));
            let (_, path) = four(&directory);
            let text = fs::read_to_string(&path).expect("read");
            let line = format!("\n{}", writer_line("probe"));
            let named = i64::try_from(text.rfind(&line).expect("named")).expect("small");
            let length = named + edge + i64::try_from(window).expect("small");
            filled(&path, usize::try_from(length).expect("after the records"));

            forget(&path);
            let again = ProgramAudit::new("probe", Some(FileSink::new(&*directory)));
            act(&again, "five");

            let verdict = &verdicts(&path, &["probe"])[0];
            assert!(verdict.whole(), "{name}: {}", verdict.said());
            assert_eq!(verdict.records, 5, "{name}");
        }
    }

    #[test]
    fn two_writers_chains_are_each_their_own() {
        let directory = scratch("two");
        let node = configure::fixture::test_cluster().node_scope(0);
        let (first, second) = (
            ProgramAudit::new("probe", Some(FileSink::new(&*directory))),
            ProgramAudit::new("probe", Some(FileSink::new(&*directory))),
        );
        second.locate(&node);
        for action in ["one", "two", "three"] {
            act(&first, action);
            act(&second, action);
        }
        let path = FileSink::new(&*directory).path();
        let written = tables(&path);
        let without = [
            &written[0],
            &written[1],
            &written[3],
            &written[4],
            &written[5],
        ];
        fs::write(&path, without.map(String::as_str).concat()).expect("written");

        let verdicts = verdicts(&path, &["probe", &node]);
        assert!(!verdicts[0].whole(), "the program's lost its second");
        assert!(verdicts[1].whole(), "{}", verdicts[1].said());
        assert_eq!(verdicts[1].records, 3);
    }

    #[test]
    fn an_interrupted_record_is_sealed_apart_and_the_chain_goes_on_from_the_last_whole_one() {
        let directory = scratch("interrupted");
        let (audit, path) = four(&directory);
        act(&audit, "five");
        let written = tables(&path);
        let before: String = written[..4].concat();
        // The fifth record's write, interrupted inside a line.
        let cut = &written[4][..written[4].len() / 2];
        assert!(!cut.ends_with('\n'), "{cut}");
        fs::write(&path, format!("{before}{cut}")).expect("written");

        forget(&path);
        let again = ProgramAudit::new("probe", Some(FileSink::new(&*directory)));
        act(&again, "six");

        let bytes = fs::read(&path).expect("read");
        assert_eq!(
            &bytes[..before.len() + cut.len()],
            format!("{before}{cut}").as_bytes()
        );
        let read = parse(&bytes);
        let actions: Vec<&str> = read.entries.iter().map(|e| e.action.as_str()).collect();
        assert_eq!(actions, ["one", "two", "three", "four", "six"]);
        let six = &read.entries[4];
        assert_eq!(
            six.position,
            Some(5),
            "number 5 again: the fifth was never whole"
        );
        assert_eq!(six.previous, read.entries[3].digest, "chained to four");

        let mut entries = Vec::new();
        let fragments =
            audit::audit_store::each(&path, |entry| entries.push(entry.clone())).expect("read");
        let verdict = &chains(&entries, &["probe"])[0];
        assert!(verdict.whole(), "{}", verdict.said());
        assert_eq!(verdict.records, 5);
        let sealed = &fragments[..];
        assert_eq!(sealed.len(), 1, "{sealed:?}");
        assert!(sealed[0].sealed);
        assert_eq!(sealed[0].offset, before.len());
        assert!(sealed[0].length > cut.len());
        assert!(
            sealed[0].said().starts_with("FAILED: "),
            "{}",
            sealed[0].said()
        );
    }

    #[test]
    fn an_unfinished_end_is_said_apart_from_a_whole_chain() {
        let directory = scratch("unfinished");
        let (_, path) = four(&directory);
        let length = fs::metadata(&path).expect("there").len();
        let mut text = fs::read_to_string(&path).expect("read");
        text.push_str("[[record]]\naudit_id = \"0");
        fs::write(&path, &text).expect("written");

        let mut entries = Vec::new();
        let fragments =
            audit::audit_store::each(&path, |entry| entries.push(entry.clone())).expect("read");
        let verdict = &chains(&entries, &["probe"])[0];
        assert!(verdict.whole(), "{}", verdict.said());
        assert_eq!(verdict.records, 4);
        let open = &fragments[..];
        assert_eq!(open.len(), 1, "{open:?}");
        assert!(!open[0].sealed);
        assert_eq!(open[0].offset as u64, length);
        assert!(
            open[0].said().starts_with("UNFINISHED: "),
            "{}",
            open[0].said()
        );
    }
}
