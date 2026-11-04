mod common;

use common::*;

#[test]
fn runtime_level_filters_without_rebuilding() {
    rtlog::set_level(rtlog::Level::Warn);
    rtlog::info!("hidden {}", 1u8);
    rtlog::warn!("shown {}", 2u8);
    rtlog::error!("shown {}", 3u8);
    rtlog::set_off();
    let mut evaluated = false;
    rtlog::error!("hidden {}", {
        evaluated = true;
        4u8
    });
    assert!(!evaluated, "arguments of a disabled site are not evaluated");
    assert_eq!(rtlog::level::level(), None);
    rtlog::set_level(rtlog::Level::Trace);
    rtlog::trace!("shown {}", 5u8);

    assert_eq!(
        messages(&decode_text(&drain_all())),
        ["shown 2", "shown 3", "shown 5"]
    );
}
