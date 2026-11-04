#![no_main]

use libfuzzer_sys::fuzz_target;
use std::sync::OnceLock;
use tokio::runtime::{Builder, Runtime};
use diapason_core::exec_policy::command_validation::validate_command;

const MAX_INPUT_BYTES: usize = 2048;
const MAX_TOKENS: usize = 12;

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("failed to initialize tokio runtime for fuzzing")
    })
}

fn bounded_command(data: &[u8]) -> Vec<String> {
    let slice = if data.len() > MAX_INPUT_BYTES {
        &data[..MAX_INPUT_BYTES]
    } else {
        data
    };

    String::from_utf8_lossy(slice)
        .split_whitespace()
        .take(MAX_TOKENS)
        .map(|token| token.chars().take(128).collect())
        .collect()
}

fuzz_target!(|data: &[u8]| {
    let Ok(workspace) = tempfile::tempdir() else {
        return;
    };

    let root = workspace.path();
    let _ = std::fs::create_dir_all(root.join("nested"));
    let _ = std::fs::write(root.join("nested/seed.txt"), b"seed");
    let command = bounded_command(data);

    if !command.is_empty() {
        let _ = runtime().block_on(validate_command(&command, root, root, false));
    }
});
