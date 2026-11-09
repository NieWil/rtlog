//! Two linked sites with the same ID: `contract::write` refuses to write a
//! contract the decoder could not use. Separate test binary, because the
//! duplicate descriptors below are linked into every test in it.

use rtlog::__private::linkme;
use rtlog::{Level, Site};

// Two descriptors with identical fields hash to the same ID, as two distinct
// sites with a hash collision would.
#[linkme::distributed_slice(rtlog::site::SITES)]
#[linkme(crate = rtlog::__private::linkme)]
static FIRST: Site = Site::new(Level::Info, "m", "dup.rs", 7, 1, "x={}", 1);

#[linkme::distributed_slice(rtlog::site::SITES)]
#[linkme(crate = rtlog::__private::linkme)]
static SECOND: Site = Site::new(Level::Info, "m", "dup.rs", 7, 1, "x={}", 1);

#[test]
fn a_site_id_collision_is_reported_and_nothing_is_written() {
    let (a, b) = rtlog::site::find_collision().expect("the duplicate is found");
    assert_eq!(a.id, b.id);
    assert_eq!(a.id, FIRST.id);
    assert!(!core::ptr::eq(a, b), "two distinct descriptors");

    let path = std::env::temp_dir().join(format!("rtlog-collision-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let err = rtlog::contract::write(&path).unwrap_err();
    match &err {
        rtlog::contract::ContractError::Collision { id, a, b } => {
            assert_eq!(*id, SECOND.id);
            assert_eq!((a.as_str(), b.as_str()), ("dup.rs:7", "dup.rs:7"));
        }
        other => panic!("expected a collision, got {other:?}"),
    }
    assert_eq!(
        err.to_string(),
        format!(
            "site ID collision {:#010x}: dup.rs:7 and dup.rs:7",
            FIRST.id
        )
    );
    assert!(!path.exists(), "no contract file is written");
}
