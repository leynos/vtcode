#![no_main]

use libfuzzer_sys::fuzz_target;
use diapason_core::command_safety::dangerous_commands::command_might_be_dangerous;

const MAX_TOKENS: usize = 8;
const MAX_INPUT_BYTES: usize = 128;

fn bounded_tokens(data: &[u8]) -> Vec<String> {
    let slice = if data.len() > MAX_INPUT_BYTES {
        &data[..MAX_INPUT_BYTES]
    } else {
        data
    };
    let text = String::from_utf8_lossy(slice);
    text.split_whitespace()
        .take(MAX_TOKENS)
        .map(|s| {
            let t: String = s.chars().take(32).collect();
            t
        })
        .collect()
}

fuzz_target!(|data: &[u8]| {
    let command = bounded_tokens(data);
    let _ = command_might_be_dangerous(&command);
});
