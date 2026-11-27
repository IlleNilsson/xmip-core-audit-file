# xmip-core-audit-file

The `file` technology of `xmip-core-audit`: records appended as `[[record]]`
TOML tables to `audit.toml` in a directory, every Xmip program's default
sink (ADR-0062). `FileSink` is an `audit::AuditSink`; the program that runs
hands it to its `ProgramAudit` as it starts — `FileSink::stated(directory)`,
the directory it was told, else the one `XMIP_AUDIT_DIRECTORY` names, else
none and the operating system's log. Where that directory is, is audit's
one rule (`audit::audit_store::directory`), which audit's reader reads the
file by: the sink writes where every surface reads.

| Item | What it is |
| --- | --- |
| `FileSink` | The sink: `new(directory)`, `stated(directory)`, `path()`, and `write`, which says a record written only once it is on the disk |
| `writer` | The file's one writer in a process: every record waiting appended in one write and one durable flush, so records handed over together share it; the file read only where this process did not leave it as it stands (ADR-0062, amendment 2026-11-13) |
| `chain` | Where each writer's audit chain stands in the file, read under `audit.lock`, the operating system's lock on a file beside it: what was appended since this process last looked, or, for a writer it has not met, the file read back from its end to that writer's last record (ADR-0070 clause 5) |
| `temporary` | Behind the `test-support` feature: `Folder`, a test's own folder its audit is written in, a `file::temporary::Folder` removed only once audit's keeper kept every record handed to it before (ADR-0065, amendment 2026-11-20) |

**A test's audit folder.** A record made where its caller must not wait
for a disk, an Event subscription's close record among them, is handed to
audit's keeper and written after the caller went on; a program's stop is
kept after it, and a test records no stop. So a test that let go of its
folder at once had the keeper make the folder and its audit file again,
and the folder outlived its test. A test whose audit takes records through
the keeper — an Event subscription's, a sync plane's — makes its folder
with `temporary::Folder`, first, so that it is dropped last; only a
dev-dependency turns `test-support` on. Held by `tests/temporary.rs`, a
test binary of its own so the keeper's inbox is its alone.

**The chain under the lock.** Every record is appended in its writer's
audit chain — its number and the digest of the record before it, by
`xcore::audit_chain` — holding `audit.lock`, so two programs appending at
once, or one started again, each go on from their writer's last whole
record. **The sealed fragment.** A table a write left unfinished is never
written onto: holding the lock, the next append keeps its bytes as written
and ends them with audit's seal (`audit::audit_fragment`), so they stand
apart, in no chain, and a verification says them apart from the chains.

The record, its table, the chain's walk, the seal's words and the reader
are `xmip-core-audit`'s; this crate appends.

The timing program, `tests/measured`: a record the sink keeps is on the
disk within a millisecond of the disk's own durable write of as many bytes
(`written.rs`; the millisecond rule, `CONTRIBUTING.md`).

`architecture.toml` carries the maturity.

## Verification

The included workflow is manual-only and calls the versioned shared workflow at
`IlleNilsson/.github@v1`.
