//! A process's first record of a writer its file holds far back — as a
//! System Tests far end's last record lies behind every record a roll's
//! nodes appended since — held to the millisecond rule beside the disk's
//! own reading of the file and durable write of as many bytes: finding
//! where the writer's chain stands costs reading the file back, never
//! parsing it. Until 2026-11-25 each window read back was parsed whole, 1.5
//! to 3 s for a far end's first record in a 7 MB file.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use audit::AuditSink;
use audit::audit_record::AuditRecord;
use audit::origin::Origin;
use xcore::fastest::FIVE;
use xcore::latency::spread;
use xcore::{AuditId, ExecutionPhase, Severity};
use xmip_core_audit_file::FileSink;

use super::scratch;

/// How many writers' first records are measured.
const ROUNDS: usize = 8 * FIVE;

/// How much of other writers' records lies after each writer's last.
const BEHIND: usize = 4 << 20;

/// The bound: a millisecond.
const BOUND: Duration = Duration::from_millis(1);

/// A record of the program `program`, which is its writer.
fn record(program: &str, number: usize) -> AuditRecord {
    AuditRecord {
        audit_id: AuditId::new(u128::try_from(number).unwrap_or_default()),
        origin: Origin::here(program),
        scope: None,
        action: "start".to_string(),
        phase: ExecutionPhase::Begin,
        severity: Severity::Information,
        timestamp_unix_nanos: 0,
        message: None,
        properties: [("external".to_string(), format!("far end {number}"))].into(),
    }
}

/// The writer measured in round `number`.
fn writer(number: usize) -> String {
    format!("far-end-{number}")
}

/// The disk's own reading of the file at `path`, and durable write of
/// `bytes` bytes to `beside`: each opened, read or appended and flushed, and
/// closed.
fn own(path: &Path, beside: &Path, bytes: usize) -> Duration {
    let began = Instant::now();
    let mut read = Vec::new();
    File::open(path)
        .and_then(|mut file| file.read_to_end(&mut read))
        .expect("read");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(beside)
        .expect("opened");
    file.write_all(&vec![b'#'; bytes]).expect("written");
    file.sync_data().expect("flushed");
    drop(file);
    began.elapsed()
}

#[test]
fn a_writers_first_record_far_back_is_on_the_disk_within_a_millisecond_of_reading_the_file() {
    let directory = scratch("first");
    let sink = FileSink::new(&*directory);
    // Each writer's last record at the file's start and the others' after
    // them, written as the sink writes them but not by this process, which
    // has met none of the writers.
    let mut text = String::new();
    for number in 0..ROUNDS {
        let program = writer(number);
        text.push_str(&record(&program, number).chained(&program, 1, &[0; 32]).0);
    }
    let mut previous = [0; 32];
    for position in 1.. {
        if text.len() > BEHIND {
            break;
        }
        let (table, digest) = record("another", 0).chained("another", position, &previous);
        text.push_str(&table);
        previous = digest;
    }
    fs::write(sink.path(), &text).expect("written");
    let beside = directory.join("beside.toml");
    let (mut kept, mut theirs) = (Vec::new(), Vec::new());
    for number in 0..ROUNDS {
        let before = fs::metadata(sink.path()).expect("there").len();
        let began = Instant::now();
        sink.write(&record(&writer(number), number))
            .expect("written");
        kept.push(began.elapsed());
        let grew = fs::metadata(sink.path()).expect("there").len() - before;
        theirs.push(own(
            &sink.path(),
            &beside,
            usize::try_from(grew).unwrap_or_default(),
        ));
    }
    let kept = spread("a writer's first record, its last far back", kept).fastest_of_five;
    let theirs =
        spread("the disk's own read of the file and durable write", theirs).fastest_of_five;
    assert!(
        kept < theirs + BOUND,
        "a first record took {kept:?}, the disk's own read and write {theirs:?}"
    );
}
