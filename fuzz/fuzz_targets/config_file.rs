//! A configuration file as the service loads it, then checks it.
#![no_main]

use libfuzzer_sys::fuzz_target;
use radovid::config::Config;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(config) = toml::from_str::<Config>(text) {
        let _ = config.validate();
    }
});
