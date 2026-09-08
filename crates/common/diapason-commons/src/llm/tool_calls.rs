//! Tool-call wire types and resilient JSON argument recovery.

use serde::{Deserialize, Serialize};

/// Universal tool call that matches `OpenAI`, `Anthropic`, and `Gemini` specifications.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique identifier for this tool call (e.g., "call_123")
    pub id: String,

    /// The type of tool call: "function", "custom" (GPT-5 freeform), or other
    #[serde(rename = "type")]
    pub call_type: String,

    /// Function call details (for function-type tools)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<FunctionCall>,

    /// Raw text payload (for custom freeform tools in GPT-5)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,

    /// Gemini-specific thought signature for maintaining reasoning context
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

/// Function call within a tool call
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    /// Optional namespace for grouped or deferred tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    /// The name of the function to call
    pub name: String,

    /// The arguments to pass to the function, as a JSON string
    pub arguments: String,
}

impl ToolCall {
    /// Creates a function tool call without a namespace.
    pub fn function(id: String, name: String, arguments: String) -> Self {
        Self::function_with_namespace(id, None, name, arguments)
    }

    /// Creates a function tool call with an optional namespace.
    pub fn function_with_namespace(id: String, namespace: Option<String>, name: String, arguments: String) -> Self {
        Self {
            id,
            call_type: "function".to_owned(),
            function: Some(FunctionCall { namespace, name, arguments }),
            text: None,
            thought_signature: None,
        }
    }

    /// Creates a custom tool call with a raw `GPT-5` freeform payload.
    pub fn custom(id: String, name: String, text: String) -> Self {
        Self {
            id,
            call_type: "custom".to_owned(),
            function: Some(FunctionCall { namespace: None, name, arguments: text.clone() }),
            text: Some(text),
            thought_signature: None,
        }
    }

    /// Returns true when this tool call uses GPT-5 custom/freeform semantics.
    pub fn is_custom(&self) -> bool {
        self.call_type == "custom"
    }

    /// Returns the tool name when the call includes function details.
    pub fn tool_name(&self) -> Option<&str> {
        self.function.as_ref().map(|function| function.name.as_str())
    }

    /// Returns the raw payload text exactly as emitted by the model.
    pub fn raw_input(&self) -> Option<&str> {
        self.text
            .as_deref()
            .or_else(|| self.function.as_ref().map(|function| function.arguments.as_str()))
    }

    /// Parses arguments as JSON for a function-type tool.
    ///
    /// # Errors
    ///
    /// Returns a JSON error when no function payload is present or recovery fails.
    pub fn parsed_arguments(&self) -> Result<serde_json::Value, serde_json::Error> {
        if let Some(ref func) = self.function {
            parse_tool_arguments(&func.arguments)
        } else {
            // Return an error by trying to parse invalid JSON
            serde_json::from_str("")
        }
    }

    /// Returns the execution payload for this tool call.
    ///
    /// Function tools keep their JSON semantics. Custom tools execute with their
    /// raw text payload wrapped as a JSON string value so freeform inputs can
    /// flow through the existing tool pipeline.
    ///
    /// # Errors
    ///
    /// Returns a JSON error when a function tool has invalid arguments.
    pub fn execution_arguments(&self) -> Result<serde_json::Value, serde_json::Error> {
        if self.is_custom() {
            return Ok(serde_json::Value::String(self.raw_input().unwrap_or_default().to_string()));
        }

        self.parsed_arguments()
    }

    /// Validates that this tool call is properly formed.
    ///
    /// # Errors
    ///
    /// Returns a descriptive validation error for malformed tool-call data.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() {
            return Err("Tool call ID cannot be empty".to_owned());
        }

        match self.call_type.as_str() {
            "function" => {
                if let Some(func) = &self.function {
                    if func.name.is_empty() {
                        return Err("Function name cannot be empty".to_owned());
                    }
                    // Validate that arguments is valid JSON for function tools
                    if let Err(e) = self.parsed_arguments() {
                        return Err(format!("Invalid JSON in function arguments: {e}"));
                    }
                } else {
                    return Err("Function tool call missing function details".to_owned());
                }
            }
            "custom" => {
                // For custom tools, we allow raw text payload without JSON validation
                if let Some(func) = &self.function {
                    if func.name.is_empty() {
                        return Err("Custom tool name cannot be empty".to_owned());
                    }
                } else {
                    return Err("Custom tool call missing function details".to_owned());
                }
            }
            _ => return Err(format!("Unsupported tool call type: {}", self.call_type)),
        }

        Ok(())
    }
}

fn parse_tool_arguments(raw_arguments: &str) -> Result<serde_json::Value, serde_json::Error> {
    let trimmed = raw_arguments.trim();
    match serde_json::from_str(trimmed) {
        Ok(parsed) => Ok(parsed),
        Err(primary_error) => {
            if let Some(candidate) = extract_balanced_json(trimmed)
                && let Ok(parsed) = serde_json::from_str(candidate)
            {
                return Ok(parsed);
            }
            if let Some(candidate) = repair_tag_polluted_json(trimmed)
                && let Ok(parsed) = serde_json::from_str(&candidate)
            {
                return Ok(parsed);
            }
            if let Some(repaired) = close_incomplete_json_prefix(trimmed)
                && let Ok(parsed) = serde_json::from_str(&repaired)
            {
                return Ok(parsed);
            }
            Err(primary_error)
        }
    }
}

fn extract_balanced_json(input: &str) -> Option<&str> {
    let start = input.find(['{', '['])?;
    let opening = input.as_bytes().get(start).copied()?;
    let closing = match opening {
        b'{' => b'}',
        b'[' => b']',
        _ => return None,
    };

    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, ch) in input.get(start..)?.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            _ if ch as u32 == opening as u32 => depth += 1,
            _ if ch as u32 == closing as u32 => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let end = start + offset + ch.len_utf8();
                    return input.get(start..end);
                }
            }
            _ => {}
        }
    }

    None
}

fn repair_tag_polluted_json(input: &str) -> Option<String> {
    let start = input.find(['{', '['])?;
    let candidate = input.get(start..)?;
    let boundary = find_provider_markup_boundary(candidate)?;
    if boundary == 0 {
        return None;
    }

    close_incomplete_json_prefix(candidate.get(..boundary)?.trim_end())
}

fn find_provider_markup_boundary(input: &str) -> Option<usize> {
    const PROVIDER_MARKERS: &[&str] = &[
        "<</",
        "</parameter>",
        "</invoke>",
        "</minimax:tool_call>",
        "<minimax:tool_call>",
        "<parameter name=\"",
        "<invoke name=\"",
        "<tool_call>",
        "</tool_call>",
    ];

    input.char_indices().find_map(|(offset, _)| {
        let rest = input.get(offset..)?;
        PROVIDER_MARKERS.iter().any(|marker| rest.starts_with(marker)).then_some(offset)
    })
}

fn close_incomplete_json_prefix(prefix: &str) -> Option<String> {
    if prefix.is_empty() {
        return None;
    }

    let mut repaired = String::with_capacity(prefix.len() + 8);
    let mut expected_closers = Vec::new();
    let mut in_string = false;
    let mut escaped = false;

    for ch in prefix.chars() {
        repaired.push(ch);

        if in_string {
            if escaped {
                escaped = false;
                continue;
            }

            match ch {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => expected_closers.push('}'),
            '[' => expected_closers.push(']'),
            '}' | ']' if expected_closers.pop() != Some(ch) => return None,
            '}' | ']' => {}
            _ => {}
        }
    }

    if in_string {
        repaired.push('"');
    }
    for closer in expected_closers.drain(..) {
        repaired.push(closer);
    }

    Some(repaired)
}
