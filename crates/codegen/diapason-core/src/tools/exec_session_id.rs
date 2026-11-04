use diapason_macros::StringNewtype;
use serde::Deserialize;
use serde::Serialize;

/// Logical identifier for a Diapason exec session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, StringNewtype)]
#[serde(transparent)]
pub struct ExecSessionId(String);
