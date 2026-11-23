//! Defines subagent run state, outcomes, and the data shared by controller
//! components.
//!
//! The module is split by contract: `status` holds the projections adapters
//! and provider tools consume, `snapshot` the session-scoped views that carry
//! live thread data, `ledger_view` the background record and its projections,
//! and `foreground` the controller's own run state. Every existing path keeps
//! resolving through the re-exports below.
mod foreground;
mod ledger_view;
mod snapshot;
mod status;

pub(crate) use self::foreground::ChildRunRequest;
pub use self::foreground::{ChildRecord, ChildRunResult, ControllerState, StatusEntryBuilder, TurnDelegationHints};
pub use self::ledger_view::{
    BackgroundLiveness, BackgroundLivenessEntry, BackgroundRecord, BackgroundSubprocessSnapshot,
};
pub use self::snapshot::{SubagentInvocationContext, SubagentThreadSnapshot};
pub use self::status::{
    BackgroundSubprocessEntry, BackgroundSubprocessStatus, SendInputRequest, SpawnAgentRequest,
    SpawnBackgroundSubprocessRequest, SubagentInputItem, SubagentProgressObserver, SubagentStatus, SubagentStatusEntry,
};
