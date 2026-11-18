//! What a client makes of whatever a service sends first, and of whatever a person types or
//! pastes as the address of one: nothing panics, a name shown is bounded and free of control
//! characters, and an address that is accepted is canonical and a service identity.
#![no_main]

use libfuzzer_sys::fuzz_target;
use radovid_client::{MAX_NAME_CHARS, judge_message};
use radovid_proto::{matchmaker_identity, service_address};

fuzz_target!(|data: &[u8]| {
    for seq in [0, 1, u32::MAX] {
        if let Ok(found) = judge_message(seq, data) {
            assert!(found.name.chars().count() <= MAX_NAME_CHARS);
            assert!(!found.name.chars().any(char::is_control));
        }
    }
    let Ok(typed) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(canonical) = service_address(typed) {
        assert_eq!(
            service_address(&canonical).as_deref(),
            Ok(canonical.as_str())
        );
        assert!(matchmaker_identity(&canonical).is_some());
        assert!(canonical.starts_with("wss://"));
    }
});
