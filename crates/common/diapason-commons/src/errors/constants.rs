//! Stable error-message constants shared by filesystem and tool operations.

/// Failure to read a file.
pub const ERR_READ_FILE: &str = "failed to read file";
/// Failure to write a file.
pub const ERR_WRITE_FILE: &str = "failed to write file";
/// Failure to read a directory.
pub const ERR_READ_DIR: &str = "failed to read directory";
/// Failure to create a directory.
pub const ERR_CREATE_DIR: &str = "failed to create directory";
/// Failure to remove a file.
pub const ERR_REMOVE_FILE: &str = "failed to remove file";
/// Failure to remove a directory.
pub const ERR_REMOVE_DIR: &str = "failed to remove directory";
/// Failure to read a directory entry.
pub const ERR_READ_DIR_ENTRY: &str = "failed to read directory entry";
/// Failure to read a file type.
pub const ERR_GET_FILE_TYPE: &str = "failed to read file type";
/// Failure to read file metadata.
pub const ERR_GET_METADATA: &str = "failed to read file metadata";
/// Failure to canonicalize a path.
pub const ERR_CANONICALIZE_PATH: &str = "failed to canonicalize path";
/// Failure to read a symbolic link.
pub const ERR_READ_SYMLINK: &str = "failed to read symlink";

// Skill/Tool errors
/// Failure to create the skills directory.
pub const ERR_CREATE_SKILLS_DIR: &str = "failed to create skills directory";
/// Failure to create a skill directory.
pub const ERR_CREATE_SKILL_DIR: &str = "failed to create skill directory";
/// Failure to read skill code.
pub const ERR_READ_SKILL_CODE: &str = "failed to read skill code";
/// Failure to write skill code.
pub const ERR_WRITE_SKILL_CODE: &str = "failed to write skill code";
/// Failure to read skill metadata.
pub const ERR_READ_SKILL_METADATA: &str = "failed to read skill metadata";
/// Failure to write skill metadata.
pub const ERR_WRITE_SKILL_METADATA: &str = "failed to write skill metadata";
/// Failure to parse skill metadata.
pub const ERR_PARSE_SKILL_METADATA: &str = "failed to parse skill metadata";
/// Failure to write skill documentation.
pub const ERR_WRITE_SKILL_DOCS: &str = "failed to write skill documentation";
/// Failure to delete a skill.
pub const ERR_DELETE_SKILL: &str = "failed to delete skill";
/// Failure to read the skills directory.
pub const ERR_READ_SKILLS_DIR: &str = "failed to read skills directory";
/// Message reported when a tool is denied or unavailable by policy.
pub const ERR_TOOL_DENIED: &str = "tool denied or unavailable by policy";

// Audit/Logging errors
/// Failure to create the audit directory.
pub const ERR_CREATE_AUDIT_DIR: &str = "Failed to create audit directory";
/// Failure to write an audit log entry.
pub const ERR_WRITE_AUDIT_LOG: &str = "failed to write audit log";

// Checkpoint/Snapshot errors
/// Failure to create the checkpoint directory.
pub const ERR_CREATE_CHECKPOINT_DIR: &str = "failed to create checkpoint directory";
/// Failure to write a checkpoint.
pub const ERR_WRITE_CHECKPOINT: &str = "failed to write checkpoint";
/// Failure to read a checkpoint.
pub const ERR_READ_CHECKPOINT: &str = "failed to read checkpoint";

// Policy errors
/// Failure to create the tool-policy configuration directory.
pub const ERR_CREATE_POLICY_DIR: &str = "Failed to create directory for tool policy config";
/// Failure to create the workspace policy directory.
pub const ERR_CREATE_WORKSPACE_POLICY_DIR: &str = "Failed to create workspace policy directory";

// Serialization errors
/// Failure to serialize skill metadata.
pub const ERR_SERIALIZE_METADATA: &str = "failed to serialize skill metadata";
/// Failure to serialize state.
pub const ERR_SERIALIZE_STATE: &str = "failed to serialize state";
/// Failure to deserialize data.
pub const ERR_DESERIALIZE: &str = "failed to deserialize data";

// IPC/SDK errors
/// Failure to create the IPC directory.
pub const ERR_CREATE_IPC_DIR: &str = "failed to create IPC directory";
/// Failure to read a request file.
pub const ERR_READ_REQUEST_FILE: &str = "failed to read request file";
/// Failure to read request JSON.
pub const ERR_READ_REQUEST_JSON: &str = "failed to read request JSON";
/// Failure to parse request JSON.
pub const ERR_PARSE_REQUEST_JSON: &str = "failed to parse request JSON";
/// Failure to parse tokenized arguments.
pub const ERR_PARSE_ARGS: &str = "failed to parse tokenized args";
/// Failure to parse de-tokenized results.
pub const ERR_PARSE_RESULT: &str = "failed to parse de-tokenized result";
