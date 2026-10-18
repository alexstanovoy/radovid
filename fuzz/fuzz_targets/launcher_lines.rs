//! A game server's stdout as radovid reads it: lines of any length, any
//! bytes, a `RESULT` of any shape.
#![no_main]

use libfuzzer_sys::fuzz_target;
use radovid::launcher::LineReader;

fuzz_target!(|data: &[u8]| {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let mut lines = LineReader::new(data);
        while lines.next().await.is_some() {}
    });
});
