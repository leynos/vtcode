# diapason-macros

Procedural macros for Diapason.

`diapason-macros` provides derive macros that eliminate boilerplate for common
patterns in the Diapason codebase.

<!-- cargo-rdme -->

## Derive macros

### `StringNewtype`

Derive macro for tuple structs wrapping a single `String` field. Generates:

- Inherent methods: `new()`, `as_str()`, `into_inner()`
- `Deref<Target = str>`
- `Borrow<str>`
- `AsRef<str>`
- `Display`
- `From<String>`, `From<&str>`, `From<Self> for String`

## Usage

```rust,ignore
use diapason_macros::StringNewtype;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize, StringNewtype)]
#[serde(transparent)]
pub struct SessionId(String);

let id = SessionId::new("abc-123");
assert_eq!(id.as_str(), "abc-123");
assert_eq!(id.to_string(), "abc-123");

let inner: String = id.into_inner();
```

## API reference

See [docs.rs/diapason-macros](https://docs.rs/diapason-macros).
