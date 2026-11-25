//! The module's timing program (`doc/governance/rust-style.md` section
//! 5c): the file sink held to a time. A plain `cargo test` leaves it out;
//! the landing runs it after every other test of the module, alone, one
//! test at a time (`cargo test --test measured --all-features --
//! --test-threads=1`), so what is measured is Xmip's work and not its
//! neighbors':
//!
//! - `written.rs`: a record the file sink keeps is on the disk within a
//!   millisecond of the disk's own durable write of as many bytes.
//! - `first.rs`: a process's first record of a writer its file holds far
//!   back is on the disk within a millisecond of the disk's own reading of
//!   the file and durable write of as many bytes.

mod first;
mod written;

use file::temporary::Folder;

fn scratch(name: &str) -> Folder {
    Folder::new(&format!("audit-file-{name}"))
}
