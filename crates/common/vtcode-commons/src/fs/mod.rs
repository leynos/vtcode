#![expect(
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::let_underscore_must_use,
    unused_results,
    reason = "Filesystem helpers validate path lengths and intentionally ignore local cleanup results."
)]

//! Shared asynchronous and synchronous filesystem utilities.

mod operations;
mod utilities;

pub use operations::{
    canonicalize_with_context, canonicalize_with_context_async, create_dir_all_async, create_private_file,
    ensure_dir_exists, parse_json_or_default, parse_json_with_context, read_file_with_context, read_json_file,
    read_private_file_no_follow, read_private_json_file, read_to_string_async, remove_file_async, rename_async,
    serialize_json_pretty_with_context, serialize_json_with_context, try_parse_json, try_parse_json_value,
    with_private_file_lock, write_async, write_file_atomic_with_context, write_file_with_context, write_json_file,
    write_private_file_atomic, write_private_file_atomic_if_absent, write_private_json_file,
};
pub use utilities::{
    ensure_dir_exists_sync, is_image_path, is_windows_absolute_path, read_file_with_context_sync, read_json_file_sync,
    trim_trailing_image_path, trim_trailing_image_path_str, unescape_whitespace, write_file_with_context_sync,
    write_json_file_sync,
};
