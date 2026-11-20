//! A test's audit folder outlives no record handed to audit's keeper: one
//! kept after the test dropped its folder made the folder and its audit
//! file again, so the folder outlived its test (ADR-0065, amendment
//! 2026-11-20).
//!
//! A test binary of its own, so what waits in the keeper's inbox is this
//! test's alone and the count it reads is exact.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use audit::keeper;
use audit::program_audit::ProgramAudit;
use xcore::{ExecutionPhase, Severity};
use xmip_core_audit_file::FileSink;
use xmip_core_audit_file::temporary::Folder;

/// How long the keeper is held at most, should the test go wrong.
const HELD: Duration = Duration::from_secs(10);

#[test]
fn a_record_kept_after_the_test_let_go_of_its_folder_does_not_make_it_again() {
    let folder = Folder::new("audit-file-temporary");
    let path = folder.to_path_buf();
    let audit = ProgramAudit::new("xmip-core-audit-file tests", Some(FileSink::new(&*folder)));

    // The keeper is busy with a record before it, as it is under load, so
    // the subscription's close record waits in the inbox.
    let (started, busy) = mpsc::channel();
    let (release, held) = mpsc::channel::<()>();
    keeper::later(move || {
        let _ = started.send(());
        let _ = held.recv_timeout(HELD);
    });
    busy.recv_timeout(HELD).expect("the keeper took the first");
    keeper::later(move || {
        let _ = audit.record(
            "event.unsubscribe",
            ExecutionPhase::Finished,
            Severity::Information,
            Some("closed"),
            BTreeMap::new(),
        );
    });

    // The keeper goes on once the drop waits for it, its settle queued
    // behind the close record, or once the drop returned without waiting.
    let dropped = Arc::new(AtomicBool::new(false));
    let releasing = {
        let dropped = Arc::clone(&dropped);
        thread::spawn(move || {
            while keeper::waiting() < 2 && !dropped.load(Ordering::SeqCst) {
                thread::yield_now();
            }
            let _ = release.send(());
        })
    };
    drop(folder);
    dropped.store(true, Ordering::SeqCst);
    releasing.join().expect("released");
    keeper::settle();

    assert!(
        !path.exists(),
        "{} was made again by the record kept after it was dropped",
        path.display()
    );
}
