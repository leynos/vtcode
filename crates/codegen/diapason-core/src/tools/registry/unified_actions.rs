use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandSessionAction {
    Run,
    Write,
    Poll,
    Wait,
    Continue,
    Inspect,
    List,
    Close,
    Code,
}
