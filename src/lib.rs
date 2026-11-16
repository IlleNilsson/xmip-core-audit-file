//! The file sink: records appended to one TOML file in a directory — the
//! default sink of every Xmip program (ADR-0062), and the `file` technology
//! of `xmip-core-audit` in `architecture.toml`. The program that runs hands
//! it to its `audit::program_audit::ProgramAudit` as it starts; audit knows
//! it only as an [`AuditSink`].
//!
//! Which directory is the audit capability's one rule, which its reader
//! reads by too ([`audit_store::directory`]): the one a program was told
//! (its configuration, a node's area), else `XMIP_AUDIT_DIRECTORY`, else
//! none — and with none, the program's audit sends the record to the
//! operating system's log it was handed. The estate's own tooling sets the
//! variable to `.local-work/audit` for everything it starts.
//!
//! Each record is appended in its writer's audit chain, holding the lock
//! beside the file ([`chain`]; ADR-0070 clause 5), and never onto a table a
//! write left unfinished: that is sealed apart first, its bytes kept as
//! written (`audit::audit_fragment`). A record is written only once it is
//! on the disk: the file's one writer in this process ([`writer`]) appends
//! what waits in one write and flushes it durably before any of it is said
//! written, so records handed over together share one flush (ADR-0062,
//! amendment 2026-11-13).

use std::path::{Path, PathBuf};

use audit::audit_record::AuditRecord;
use audit::audit_store::{self, FILE_NAME};
use audit::{AuditError, AuditSink};

mod chain;
mod writer;

/// Records appended to [`FILE_NAME`] in one directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileSink {
    directory: PathBuf,
}

impl FileSink {
    /// A sink over `directory`, created on the first write.
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    /// The sink a program writes to: in the directory
    /// [`audit_store::directory`] decides for `stated`, else none.
    #[must_use]
    pub fn stated(stated: Option<&Path>) -> Option<Self> {
        audit_store::directory(stated).map(Self::new)
    }

    /// The file the records go to.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.directory.join(FILE_NAME)
    }
}

impl AuditSink for FileSink {
    /// `record` appended in its writer's chain and on the disk, or why not.
    fn write(&self, record: &AuditRecord) -> Result<(), AuditError> {
        writer::of(&self.directory).keep(record).map_err(|error| {
            AuditError::new(format!(
                "{} could not be written: {error}",
                self.path().display()
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use audit::audit_chain::chains;
    use audit::audit_entry::parse;
    use audit::origin::Origin;
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::Arc;
    use xcore::{AuditId, ExecutionPhase, Severity};

    fn record(action: &str) -> AuditRecord {
        AuditRecord {
            audit_id: AuditId::new(7),
            origin: Origin::here("probe"),
            scope: None,
            action: action.to_string(),
            phase: ExecutionPhase::Begin,
            severity: Severity::Information,
            timestamp_unix_nanos: 0,
            message: None,
            properties: BTreeMap::new(),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("xmip-audit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        directory
    }

    #[test]
    fn records_append_to_one_file_that_reads_as_toml_tables() {
        let directory = scratch("append");
        let sink = FileSink::new(&directory);

        sink.write(&record("start")).expect("written");
        sink.write(&record("stop")).expect("written");

        let text = fs::read_to_string(sink.path()).expect("read");
        assert_eq!(text.matches("[[record]]").count(), 2, "{text}");
        assert!(text.contains("action = \"start\""), "{text}");
        assert!(text.contains("action = \"stop\""), "{text}");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn records_handed_over_at_once_are_each_kept_once_in_one_whole_chain() {
        let directory = scratch("together");
        let sink = Arc::new(FileSink::new(&directory));
        let (threads, each) = (8, 25);
        let writing: Vec<_> = (0..threads)
            .map(|thread| {
                let sink = Arc::clone(&sink);
                std::thread::spawn(move || {
                    for number in 0..each {
                        let one = AuditRecord {
                            audit_id: AuditId::new(
                                u128::try_from(thread * 1_000 + number).expect("small"),
                            ),
                            ..record(&format!("t{thread}-{number}"))
                        };
                        sink.write(&one).expect("written");
                    }
                })
            })
            .collect();
        for thread in writing {
            thread.join().expect("wrote");
        }

        let bytes = fs::read(sink.path()).expect("read");
        let read = parse(&bytes);
        assert_eq!(read.entries.len(), threads * each);
        let mut actions: Vec<&str> = read.entries.iter().map(|e| e.action.as_str()).collect();
        actions.sort_unstable();
        actions.dedup();
        assert_eq!(actions.len(), threads * each, "each kept once");
        let verdict = &chains(&read.entries, &["probe"])[0];
        assert!(verdict.whole(), "{}", verdict.said());
        assert_eq!(verdict.records, u64::try_from(threads * each).expect("few"));
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_directory_that_cannot_be_made_is_refused_with_the_path() {
        let directory = scratch("blocked");
        fs::create_dir_all(&directory).expect("made");
        let blocker = directory.join("a-file");
        fs::write(&blocker, b"not a directory").expect("written");

        let error = FileSink::new(blocker.join("audit"))
            .write(&record("start"))
            .expect_err("a file stands where the directory would be");

        assert!(
            error.to_string().contains("could not be written"),
            "{error}"
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn the_sink_writes_where_the_reader_reads() {
        let stated = FileSink::stated(Some(Path::new("stated"))).expect("stated");
        assert_eq!(stated.path(), Path::new("stated").join(FILE_NAME));
        assert_eq!(
            Some(stated.path()),
            audit_store::stated(Some(Path::new("stated")))
        );

        // Blank is not a statement; what is left is the variable, which this
        // test neither sets nor clears, so it only asks the answer be one.
        let blank = FileSink::stated(Some(Path::new("")));
        let from_variable = FileSink::stated(None);
        assert_eq!(blank, from_variable);
    }
}
