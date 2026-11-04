#![no_main]

use libfuzzer_sys::fuzz_target;
use std::path::Path;
use diapason_core::exec_policy::PolicyParser;

const MAX_INPUT_BYTES: usize = 4096;

fn bounded_input(data: &[u8]) -> String {
    let slice = if data.len() > MAX_INPUT_BYTES {
        &data[..MAX_INPUT_BYTES]
    } else {
        data
    };
    String::from_utf8_lossy(slice).into_owned()
}

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }

    let mode = data[0] % 3;
    let input = bounded_input(&data[1..]);
    let parser = PolicyParser::default();

    // Fuzz the public policy-loading surface; the format is selected by the
    // file extension (toml/json/line-based) exactly as production callers do.
    // load_from_content exercises the full parse -> rule -> policy pipeline,
    // including Policy::add_prefix_rule, so the private per-format parsers do
    // not need to be exposed for fuzzing.
    let path = match mode {
        0 => Path::new("policy.rules"),
        1 => Path::new("policy.toml"),
        _ => Path::new("policy.json"),
    };
    let _ = parser.load_from_content(&input, path);
});
