//! File/folder rename execution support: descriptor-relative moves, durable events.
pub mod events;
pub(crate) mod fsops;

/// On-disk name an entry is parked under while a case-only rename needs two steps (journal
/// recovery recognises it by device and inode). Public so integration tests share one definition.
pub fn case_transit_name(dev: u64, ino: u64) -> String {
    fsops::transit_name(fsops::Ident {
        dev: dev as i64,
        ino: ino as i64,
    })
}
