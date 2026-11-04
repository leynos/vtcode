# diapason-indexer

Workspace-friendly code indexer extracted from Diapason.

`diapason-indexer` offers a lightweight alternative to heavyweight
search/indexing stacks. It recursively walks a workspace, computes per-file
hashes, and stores metadata in Markdown-friendly summaries so changes remain
easy to audit in git.

<!-- cargo-rdme start -->

Workspace-friendly file indexer and file utilities for Diapason.

`diapason-indexer` provides:

- A lightweight workspace file indexer with markdown-backed persistence
- Fast parallel fuzzy file search (via `file_search` module)
- Markdown-backed storage utilities (via `markdown_store` module)

<!-- cargo-rdme end -->

## Features

- Recursive `.gitignore`-aware workspace walking via the `ignore` crate
- Per-file content hashing for change detection
- Markdown-backed snapshot persistence (`index.md`)
- Pluggable storage and traversal filter traits

## Public entrypoints

- `SimpleIndexer` — main indexer; walk, hash, query, and persist file metadata
- `SimpleIndexerConfig` — builder for workspace root, index directory, and
  exclusion rules
- `FileIndex` — per-file metadata record (path, hash, size, timestamps)
- `IndexStorage` trait — persistence backend (default: `MarkdownIndexStorage`)
- `TraversalFilter` trait — directory/file filtering hook (default:
  `ConfigTraversalFilter`)

## Usage

```rust,ignore
use diapason_indexer::SimpleIndexer;

let mut indexer = SimpleIndexer::new("/path/to/workspace".into());
indexer.init()?;
indexer.index_directory(std::path::Path::new("/path/to/workspace"))?;

let rust_files = indexer.find_files(r"\.rs$")?;
println!("Found {} Rust files", rust_files.len());
```

## API reference

See [docs.rs/diapason-indexer](https://docs.rs/diapason-indexer).
