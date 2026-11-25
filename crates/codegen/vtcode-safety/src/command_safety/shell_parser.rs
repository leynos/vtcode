#![expect(
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "Shell parsing tracks byte offsets at character boundaries while validating operator pairs."
)]

//! Shell script parser for `bash -lc` and similar commands.
//!
//! This module parses shell commands like:
//! ```sh
//! bash -lc "git status && cargo check"
//! ```
//!
//! Into individual command vectors for independent safety checking:
//! ```text
//! [["git", "status"], ["cargo", "check"]]
//! ```
//!
//! **Phase 4 Implementation**: Uses tree-sitter for accurate bash AST parsing.
//! Falls back to basic tokenization for minimal shell syntax.

use std::borrow::Cow;
use std::sync::Mutex;
use std::sync::OnceLock;

/// Lazy-initialized tree-sitter bash parser (wrapped in Mutex for mutation)
static BASH_PARSER: OnceLock<Result<Mutex<tree_sitter::Parser>, String>> = OnceLock::new();

/// Returns whether a shell command contains syntax whose meaning depends on
/// shell expansion rather than the literal argument text.
///
/// Safety-sensitive classification must only operate on static command
/// shapes. Parameter expansion, command substitution, brace expansion,
/// globbing, and unquoted backslash escapes can otherwise turn a
/// harmless-looking token into a different executable argument at runtime.
/// Backslash escapes inside double-quoted arguments are consumed as literal
/// argument syntax so patterns such as `rg -n "\\[profile"` remain classifiable.
pub fn contains_dynamic_shell_syntax(command: &str) -> bool {
    enum ShellQuote {
        Single,
        Double,
    }

    let mut quote: Option<ShellQuote> = None;
    let mut characters = command.chars();

    while let Some(character) = characters.next() {
        match quote {
            Some(ShellQuote::Single) => {
                if character == '\'' {
                    quote = None;
                }
            }
            Some(ShellQuote::Double) => match character {
                '"' => quote = None,
                '$' | '`' => return true,
                '\\' => {
                    // Backslash escapes inside double quotes are literal
                    // argument syntax. Consume the escaped character so an
                    // escaped quote cannot incorrectly end the quoted region;
                    // unquoted escapes remain rejected below because they can
                    // alter the command token or its shell structure.
                    if characters.next().is_none() {
                        return true;
                    }
                }
                _ => {}
            },
            None => match character {
                '\'' => quote = Some(ShellQuote::Single),
                '"' => quote = Some(ShellQuote::Double),
                '\\' | '$' | '`' | '{' | '}' | '*' | '?' | '[' | ']' => return true,
                _ => {}
            },
        }
    }

    quote.is_some()
}

/// Returns whether a `find` command contains shell syntax that can change the
/// literal option tokens after approval-time tokenization.
pub fn contains_dynamic_find_syntax(script: &str) -> bool {
    FindScript(script).contains_dynamic_syntax()
}

#[derive(Clone, Copy)]
struct FindScript<'a>(&'a str);

impl FindScript<'_> {
    fn contains_dynamic_syntax(self) -> bool {
        match parse_shell_commands_tree_sitter(self.0) {
            Ok(commands) => commands.iter().any(|command| FindCommand(command).contains_dynamic_syntax()),
            Err(_) => self.raw_script_might_be_dynamic(),
        }
    }

    fn raw_script_might_be_dynamic(self) -> bool {
        // Be conservative when the grammar cannot identify the command shape: a
        // raw script containing a find invocation and dynamic syntax must not pass
        // preflight just because parsing was incomplete.
        let has_find_invocation = self.0.split_whitespace().any(|word| {
            let command = word.trim_matches(|character: char| !character.is_ascii_alphanumeric() && character != '/');
            base_command_name(command) == "find"
        });
        has_find_invocation && contains_dynamic_shell_syntax(self.0)
    }
}

#[derive(Clone, Copy)]
struct FindCommand<'a>(&'a [String]);

impl FindCommand<'_> {
    fn contains_dynamic_syntax(self) -> bool {
        let is_find_command = self.0.first().is_some_and(|program| base_command_name(program) == "find");
        if !is_find_command {
            return false;
        }

        let mut classifier = FindExecSyntaxClassifier::default();
        self.0
            .iter()
            .any(|word| classifier.contains_dynamic_syntax(FindExecArgument(word)))
    }
}

#[derive(Clone, Copy)]
struct FindExecArgument<'a>(&'a str);

#[derive(Default)]
struct FindExecSyntaxClassifier {
    context: FindCommandContext,
    /// Whether the previous token contained the `{}` placeholder, which `find`
    /// requires before `+` terminates an `-exec` expression.
    after_placeholder: bool,
    /// How many operands a predicate such as `-name` still consumes before any
    /// action can start. `-fprintf` and `-fprint` take more than one.
    predicate_operands_remaining: u8,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum FindCommandContext {
    #[default]
    Expression,
    ExecProgram,
    ExecArguments,
    EnvArguments,
    EnvOptionArgument,
    EnvCommand,
    ShellArguments,
    ShellCommand,
}

impl FindExecSyntaxClassifier {
    fn contains_dynamic_syntax(&mut self, argument: FindExecArgument<'_>) -> bool {
        let word = argument.0;

        // Option and program comparisons use the literal token value, because
        // quoting that performs no expansion does not change what Bash runs.
        // The raw token still drives `contains_dynamic_shell_syntax` so any
        // expansion stays rejected.
        let literal = strip_literal_shell_quoting(word);
        let previous_was_placeholder = self.after_placeholder;
        // `find` only accepts the `{} +` form when the placeholder is the whole
        // argument, so `x{}` must not enable a `+` terminator.
        self.after_placeholder = literal == "{}";

        // A terminator ends the action in every non-expression context, even
        // where the current context would otherwise treat the token as command
        // source (`sh -c ';'`) or an option operand (`env -u ';'`). `find`
        // receives the unquoted `;` and ends the expression there.
        if self.context != FindCommandContext::Expression
            && is_find_exec_terminator(word, &literal, previous_was_placeholder)
        {
            self.context = FindCommandContext::Expression;
            self.predicate_operands_remaining = 0;
            return false;
        }

        // A predicate such as `-name` consumes the next token as its operand,
        // so a following quoted `'-exec'` is a pattern, not an action.
        // `-fprintf`/`-fprint` take more than one operand.
        if self.context == FindCommandContext::Expression {
            if self.predicate_operands_remaining > 0 {
                self.predicate_operands_remaining -= 1;
                return contains_dynamic_shell_syntax(word);
            }

            if let Some(operands) = find_predicate_operand_count(&literal) {
                self.predicate_operands_remaining = operands;
                return contains_dynamic_shell_syntax(word);
            }
        }

        // Only the expression context starts an action. In an argument
        // position, `env -u '-exec'` consumes the token as an operand, and a
        // quoted `'-exec'` reaches the program as an ordinary argument.
        if self.context == FindCommandContext::Expression && matches!(literal.as_ref(), "-exec" | "-execdir") {
            self.context = FindCommandContext::ExecProgram;
            return false;
        }

        match self.context {
            FindCommandContext::ExecProgram => {
                if literal.contains("{}") {
                    return true;
                }

                if is_find_exec_terminator(word, &literal, previous_was_placeholder) {
                    self.context = FindCommandContext::Expression;
                } else {
                    let program = base_command_name(&literal);
                    self.context = match program {
                        "env" => FindCommandContext::EnvArguments,
                        _ if is_shell_interpreter(program) => FindCommandContext::ShellArguments,
                        _ => FindCommandContext::ExecArguments,
                    };
                }

                contains_dynamic_shell_syntax(word)
            }
            FindCommandContext::EnvArguments if is_env_split_string_option(&literal) => true,
            FindCommandContext::EnvArguments if is_env_option_with_argument(&literal) => {
                // A quoted `env` still resolves to an option, but an expansion
                // in the raw token is dynamic regardless of its literal shape.
                if contains_dynamic_shell_syntax(word) {
                    return true;
                }

                self.context = FindCommandContext::EnvOptionArgument;
                false
            }
            FindCommandContext::EnvArguments if is_env_option(&literal) => contains_dynamic_shell_syntax(word),
            FindCommandContext::EnvArguments if is_env_assignment(&literal) => {
                // Literal assignments such as `X='value'` stay static; an
                // expansion such as `X="$(id)"` does not.
                contains_dynamic_shell_syntax(word)
            }
            FindCommandContext::EnvArguments if literal == "--" => {
                self.context = FindCommandContext::EnvCommand;
                false
            }
            FindCommandContext::EnvArguments if literal.starts_with('-') => true,
            FindCommandContext::EnvArguments if is_find_exec_terminator(word, &literal, previous_was_placeholder) => {
                self.context = FindCommandContext::Expression;
                false
            }
            FindCommandContext::EnvCommand if is_find_exec_terminator(word, &literal, previous_was_placeholder) => {
                self.context = FindCommandContext::Expression;
                false
            }
            FindCommandContext::EnvArguments | FindCommandContext::EnvCommand => {
                // `env '{}'` reaches a program position too: `find` substitutes
                // a path there, so the program is dynamic even though the
                // quoting suppresses brace expansion on the raw token.
                if literal.contains("{}") {
                    return true;
                }

                self.context = if is_shell_interpreter(&literal) {
                    FindCommandContext::ShellArguments
                } else if base_command_name(&literal) == "env" {
                    // A nested `env` keeps its own option grammar, so
                    // `env env -S 'sh -c {}'` still splits the string.
                    FindCommandContext::EnvArguments
                } else {
                    FindCommandContext::ExecArguments
                };
                contains_dynamic_shell_syntax(word)
            }
            FindCommandContext::EnvOptionArgument => {
                // The operand of `env -u` and friends is still shell syntax:
                // `env -u "$(id)"` expands at approval time.
                self.context = FindCommandContext::EnvArguments;
                contains_dynamic_shell_syntax(word)
            }
            FindCommandContext::ShellArguments if is_shell_command_option(&literal) => {
                self.context = FindCommandContext::ShellCommand;
                false
            }
            FindCommandContext::ShellCommand => {
                self.context = FindCommandContext::ExecArguments;
                literal.contains("{}") || contains_dynamic_shell_syntax(word)
            }
            context => {
                let is_static_exec_token =
                    context != FindCommandContext::Expression && (literal == "{}" || word == "\\;");
                let is_dynamic = !is_static_exec_token && contains_dynamic_shell_syntax(word);

                if context != FindCommandContext::Expression
                    && is_find_exec_terminator(word, &literal, previous_was_placeholder)
                {
                    self.context = FindCommandContext::Expression;
                } else if context != FindCommandContext::Expression && is_shell_interpreter(&literal) {
                    // A shell interpreter reached through any launcher
                    // (`timeout`, `sudo`, `chroot`, `setsid`, `busybox`, ...)
                    // is still an executable position; without this transition
                    // a following `-c {}` would stay classified as static and
                    // let `find` substitute a filename into shell source.
                    //
                    // Launchers cannot be enumerated safely, and an unknown one
                    // must not become a bypass, so this deliberately
                    // over-approximates: an interpreter named as an ordinary
                    // argument (`printf '%s' sh -c '{}'`) is also dynamic.
                    // That only costs an extra approval; the alternative would
                    // permit code execution.
                    self.context = FindCommandContext::ShellArguments;
                } else if context != FindCommandContext::Expression && base_command_name(&literal) == "env" {
                    // `env` reached behind a launcher still splits its `-S`
                    // argument into words, so `timeout 5 /usr/bin/env -S
                    // 'sh -c {}'` runs the substituted path as shell source.
                    self.context = FindCommandContext::EnvArguments;
                }

                is_dynamic
            }
        }
    }
}

fn is_env_option(argument: &str) -> bool {
    matches!(
        argument,
        "-i" | "--ignore-environment" | "-0" | "--null" | "-v" | "--debug" | "--list-signal-handling"
    ) || argument.starts_with("--unset=")
        || argument.starts_with("--chdir=")
        || argument.starts_with("--argv0=")
        || argument.starts_with("--block-signal=")
        || argument.starts_with("--default-signal=")
        || argument.starts_with("--ignore-signal=")
}

fn is_env_option_with_argument(argument: &str) -> bool {
    matches!(argument, "-u" | "--unset" | "-C" | "--chdir" | "--argv0")
}

fn is_env_split_string_option(argument: &str) -> bool {
    argument == "-S"
        || argument.starts_with("-S")
        || argument == "--split-string"
        || argument.starts_with("--split-string=")
        || argument
            .strip_prefix('-')
            .is_some_and(|short_options| !short_options.starts_with('-') && short_options.contains('S'))
}

fn is_env_assignment(argument: &str) -> bool {
    argument.split_once('=').is_some_and(|(name, _)| !name.is_empty())
}

/// Returns whether a token ends a `find` `-exec` expression.
///
/// `;` and `+` terminate from their literal value. `\;` only terminates when
/// the backslash reaches `find` unquoted: a quoted `'\;'` is passed through as
/// the two characters `\;` and does not end the expression, so it must not
/// reset the classifier into the expression context.
fn is_find_exec_terminator(raw: &str, literal: &str, previous_was_placeholder: bool) -> bool {
    // `find` ends on `+` only when the placeholder immediately precedes it;
    // elsewhere `+` is an ordinary argument to the executed command.
    raw == "\\;" || literal == ";" || (literal == "+" && previous_was_placeholder)
}

/// Returns how many operands `find` consumes after this predicate, if any.
fn find_predicate_operand_count(literal: &str) -> Option<u8> {
    // `-fprintf` takes a file name and a format, and `-fprint`/`-fls` take a
    // file name, so a single-operand model would leave the format looking like
    // the next expression token.
    match literal {
        "-fprintf" => Some(2),
        "-fprint" | "-fprint0" | "-fls" => Some(1),
        _ if matches!(
            literal,
            "-name"
                | "-iname"
                | "-path"
                | "-ipath"
                | "-wholename"
                | "-iwholename"
                | "-lname"
                | "-ilname"
                | "-regex"
                | "-iregex"
                | "-type"
                | "-xtype"
                | "-user"
                | "-group"
                | "-uid"
                | "-gid"
                | "-perm"
                | "-size"
                | "-mtime"
                | "-atime"
                | "-ctime"
                | "-mmin"
                | "-amin"
                | "-cmin"
                | "-newer"
                | "-anewer"
                | "-cnewer"
                | "-used"
                | "-links"
                | "-inum"
                | "-samefile"
                | "-fstype"
                | "-printf"
                | "-maxdepth"
                | "-mindepth"
                | "-regextype"
                | "-context"
        ) =>
        {
            Some(1)
        }
        _ => None,
    }
}

/// Removes shell quoting that performs no expansion, so a classifier can
/// compare against the literal token value.
///
/// Extraction keeps the quote characters in each token, so `'sh'`, `"sh"` and
/// the partly quoted `s'h'` all reach the classifier verbatim while Bash runs
/// `sh`. Single quotes never expand. In double quotes only `$`, backticks and
/// backslash are special: a backslash is literal unless it precedes `$`, a
/// backtick, `"`, `\` or a newline, and otherwise it escapes the next
/// character. A token whose unquoted value cannot be established is returned
/// unchanged, and the callers keep passing the raw token to
/// `contains_dynamic_shell_syntax` so expansions stay rejected.
fn strip_literal_shell_quoting(token: &str) -> Cow<'_, str> {
    if !token.contains(['\'', '"']) {
        return Cow::Borrowed(token);
    }

    let mut unquoted = String::with_capacity(token.len());
    let mut characters = token.chars();

    while let Some(character) = characters.next() {
        match character {
            '\'' => loop {
                match characters.next() {
                    Some('\'') => break,
                    Some(literal) => unquoted.push(literal),
                    None => return Cow::Borrowed(token),
                }
            },
            '"' => loop {
                match characters.next() {
                    Some('"') => break,
                    Some('\\') => match characters.next() {
                        // Inside double quotes a backslash is literal unless
                        // it escapes one of these; otherwise it is dropped and
                        // the escaped character is literal. A backslash-newline
                        // pair is a line continuation that Bash removes
                        // entirely, so both characters disappear.
                        Some(escaped @ ('$' | '`' | '"' | '\\')) => unquoted.push(escaped),
                        Some('\n') => {}
                        Some(literal) => {
                            unquoted.push('\\');
                            unquoted.push(literal);
                        }
                        None => return Cow::Borrowed(token),
                    },
                    Some('$' | '`') => return Cow::Borrowed(token),
                    Some(literal) => unquoted.push(literal),
                    None => return Cow::Borrowed(token),
                }
            },
            other => unquoted.push(other),
        }
    }

    Cow::Owned(unquoted)
}

/// Matches a shell interpreter name that has already had literal quoting
/// removed, so `s'h'` and `'sh'` both match.
fn is_shell_interpreter(program: &str) -> bool {
    matches!(
        base_command_name(program).to_ascii_lowercase().as_str(),
        "sh" | "bash" | "dash" | "zsh" | "ksh" | "ksh93" | "ash" | "mksh"
    )
}

fn is_shell_command_option(argument: &str) -> bool {
    if argument == "-c" {
        return true;
    }

    argument.strip_prefix('-').is_some_and(|short_options| {
        short_options.len() > 1 && !short_options.starts_with('-') && short_options.contains('c')
    })
}

/// Gets or initializes the bash parser
fn get_bash_parser() -> Result<&'static Mutex<tree_sitter::Parser>, String> {
    BASH_PARSER
        .get_or_init(|| {
            let mut parser = tree_sitter::Parser::new();
            let lang: tree_sitter::Language = tree_sitter_bash::LANGUAGE.into();
            parser
                .set_language(&lang)
                .map_err(|e| format!("Failed to load bash grammar: {e}"))?;
            Ok(Mutex::new(parser))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Detects the input shape documented in tree-sitter-bash issue #337, where a
/// brace before a later four-byte UTF-8 character can crash the native parser.
fn matches_tree_sitter_bash_issue_337_shape(script: &str) -> bool {
    let mut saw_open_brace = false;

    script.bytes().any(|byte| {
        if byte == b'{' {
            saw_open_brace = true;
        }

        saw_open_brace && (0xF0..=0xF4).contains(&byte)
    })
}

/// Ensures the bash tree-sitter parser is initialized.
pub fn prewarm_bash_parser() -> Result<(), String> {
    let _ = get_bash_parser()?;
    Ok(())
}

/// Parses a shell script into individual commands using tree-sitter bash grammar
///
/// # Example
/// ```text
/// Input:  "git status && cargo check"
/// Output: Ok([["git", "status"], ["cargo", "check"]])
/// ```
///
/// # Fallback
/// If tree-sitter parsing fails, falls back to simple tokenization
pub fn parse_shell_commands(script: &str) -> Result<Vec<Vec<String>>, String> {
    // Try tree-sitter parsing first
    match parse_with_tree_sitter(script, false) {
        Ok(commands) if !commands.is_empty() => return Ok(commands),
        Ok(_) => {} // Empty result, fall through to basic parsing
        Err(e) => {
            tracing::debug!("Tree-sitter bash parsing failed: {}, falling back to basic tokenization", e);
        }
    }

    // Fallback to simple tokenization
    parse_with_basic_tokenization(script)
}

/// Parses a shell script using tree-sitter bash grammar only (no fallback tokenization).
///
/// Use this when caller behavior must be strictly gated on bash grammar validity.
pub fn parse_shell_commands_tree_sitter(script: &str) -> Result<Vec<Vec<String>>, String> {
    parse_with_tree_sitter(script, true)
}

/// Returns whether every redirection in a static shell script only routes
/// command output. Input, heredoc, and descriptor-closing redirections remain
/// unsupported so progress classification can fail closed.
pub fn has_only_output_redirections(script: &str) -> bool {
    if !has_static_shell_shape_for_redirection_check(script) {
        return false;
    }

    parsed_script_has_only_output_redirections(script)
}

fn has_static_shell_shape_for_redirection_check(script: &str) -> bool {
    !matches_tree_sitter_bash_issue_337_shape(script)
        && !contains_dynamic_shell_syntax(script)
        && !contains_background_operator(script)
}

fn parsed_script_has_only_output_redirections(script: &str) -> bool {
    let Ok(parser) = get_bash_parser() else {
        return false;
    };
    let Ok(mut parser) = parser.lock() else {
        return false;
    };
    let Some(tree) = parser.parse(script, None) else {
        return false;
    };
    if tree.root_node().has_error() {
        return false;
    }

    let mut saw_redirection = false;
    if !collect_output_redirections(tree.root_node(), script, &mut saw_redirection) {
        return false;
    }
    saw_redirection
}

fn contains_background_operator(script: &str) -> bool {
    let chars = script.chars().collect::<Vec<_>>();
    let mut index = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while index < chars.len() {
        let character = chars[index];
        if character == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            index += 1;
            continue;
        }
        if character == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            index += 1;
            continue;
        }
        if in_single_quote || in_double_quote {
            index += 1;
            continue;
        }

        if character == '&' {
            let previous = index.checked_sub(1).and_then(|position| chars.get(position));
            let next = chars.get(index + 1);
            if next == Some(&'&') {
                index += 2;
                continue;
            }
            let part_of_allowed_operator = next == Some(&'&')
                || next == Some(&'>')
                || previous == Some(&'>')
                || previous == Some(&'|')
                || previous == Some(&'<');
            if !part_of_allowed_operator {
                return true;
            }
        }
        index += 1;
    }

    false
}

fn collect_output_redirections(node: tree_sitter::Node, source: &str, saw_redirection: &mut bool) -> bool {
    match node.kind() {
        "file_redirect" => {
            *saw_redirection = true;
            let Ok(text) = node.utf8_text(source.as_bytes()) else {
                return false;
            };
            if !is_output_redirection(text) {
                return false;
            }
        }
        "heredoc_redirect" | "herestring_redirect" => return false,
        _ => {}
    }

    let mut cursor = node.walk();
    node.children(&mut cursor)
        .all(|child| collect_output_redirections(child, source, saw_redirection))
}

fn is_output_redirection(text: &str) -> bool {
    let redirect = text.trim_start_matches(|character: char| character.is_ascii_digit());
    if redirect.starts_with("&>") {
        return !redirect.starts_with("&>-");
    }
    if let Some(destination) = redirect.strip_prefix(">&") {
        return destination.trim().chars().all(|character| character.is_ascii_digit());
    }

    redirect.starts_with('>') && !redirect.starts_with(">&-")
}

/// Parses shell script using tree-sitter bash grammar.
fn parse_with_tree_sitter(script: &str, reject_syntax_errors: bool) -> Result<Vec<Vec<String>>, String> {
    if matches_tree_sitter_bash_issue_337_shape(script) {
        return Err("Shell script matches the tree-sitter-bash issue #337 crash pattern".to_string());
    }

    let parser_guard = get_bash_parser()?;
    let mut parser = parser_guard.lock().map_err(|e| format!("Failed to lock parser: {e}"))?;

    let tree = parser.parse(script, None).ok_or_else(|| "Failed to parse script".to_string())?;

    let mut commands = Vec::new();
    let root = tree.root_node();
    if reject_syntax_errors && root.has_error() {
        return Err("Shell script contains syntax errors".to_string());
    }

    // Walk the full tree so commands inside loops/conditionals are remembered
    // for approval and checked for safety.  Top-level-only extraction misses
    // common read loops such as `for f in ...; do grep ...; done`.
    collect_commands_from_node(root, script, &mut commands);

    Ok(commands)
}

fn collect_commands_from_node(node: tree_sitter::Node, source: &str, commands: &mut Vec<Vec<String>>) {
    match node.kind() {
        "command" | "simple_command" => {
            if let Some(cmd) = extract_command_from_node(node, source)
                && !cmd.is_empty()
            {
                commands.push(cmd);
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                collect_commands_from_node(child, source, commands);
            }
        }
    }
}

/// Extracts a command vector from a tree-sitter node
fn extract_command_from_node(node: tree_sitter::Node, source: &str) -> Option<Vec<String>> {
    let mut command = Vec::new();
    let mut cursor = node.walk();

    // For pipeline nodes, extract the first command in the pipeline
    if node.kind() == "pipeline" {
        for child in node.children(&mut cursor) {
            if child.kind() == "command" || child.kind() == "simple_command" {
                return extract_command_from_node(child, source);
            }
        }
    }

    // Extract arguments from command node
    for child in node.children(&mut cursor) {
        if child.kind() == "command_name" {
            if let Ok(arg) = child.utf8_text(source.as_bytes()) {
                let trimmed = arg.trim();
                if !trimmed.is_empty() {
                    command.push(trimmed.to_string());
                }
            }
            continue;
        }

        if matches!(
            child.kind(),
            "word"
                | "string"
                | "raw_string"
                | "ansi_c_string"
                | "concatenation"
                | "brace_expression"
                | "expansion"
                | "simple_expansion"
                | "variable_expansion"
                | "command_substitution"
                | "process_substitution"
        ) {
            let text = child.utf8_text(source.as_bytes());
            if let Ok(arg) = text {
                let trimmed = arg.trim();
                if !trimmed.is_empty() {
                    command.push(trimmed.to_string());
                }
            }
        }
    }

    if command.is_empty() { None } else { Some(command) }
}

/// Fallback: Parses shell script with simple tokenization
fn parse_with_basic_tokenization(script: &str) -> Result<Vec<Vec<String>>, String> {
    let mut commands = Vec::new();
    let mut current_command = String::new();
    let mut in_quotes = false;
    let mut quote_char = ' ';
    let mut escaped = false;

    for ch in script.chars() {
        if escaped {
            current_command.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' => {
                escaped = true;
            }
            '\'' | '"' if !in_quotes => {
                in_quotes = true;
                quote_char = ch;
            }
            c if c == quote_char && in_quotes => {
                in_quotes = false;
            }
            '&' | '|' | ';' if !in_quotes => {
                if !current_command.trim().is_empty()
                    && let Ok(cmd) = tokenize_command(&current_command)
                {
                    commands.push(cmd);
                }
                current_command.clear();
            }
            _ => current_command.push(ch),
        }
    }

    if !current_command.trim().is_empty()
        && let Ok(cmd) = tokenize_command(&current_command)
    {
        commands.push(cmd);
    }

    Ok(commands)
}

/// Splits a command string into arguments
/// Respects quoted strings and escapes
fn tokenize_command(cmd: &str) -> Result<Vec<String>, String> {
    shell_words::split(cmd).map_err(|err| format!("failed to tokenize command: {err}"))
}

/// Parses `bash -lc "script"` style invocations
///
/// # Example
/// ```text
/// Input:  vec!["bash", "-lc", "git status && rm /"]
/// Output: Some([["git", "status"], ["rm", "/"]])
/// ```
pub fn parse_bash_lc_commands(command: &[String]) -> Option<Vec<Vec<String>>> {
    if command.is_empty() {
        return None;
    }

    let cmd_name = command[0].as_str();
    let base_cmd = std::path::Path::new(cmd_name)
        .file_name()
        .and_then(|osstr| osstr.to_str())
        .unwrap_or("");

    if base_cmd != "bash" && base_cmd != "zsh" && base_cmd != "sh" {
        return None;
    }

    // Look for -lc or -c pattern
    for window in command.windows(2) {
        if matches!(window[0].as_str(), "-lc" | "-c" | "-il" | "-ic") {
            let script = &window[1];
            return parse_shell_commands(script).ok();
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn tokenize_simple_command() {
        let cmd = "git status";
        let tokens = tokenize_command(cmd).unwrap();
        assert_eq!(tokens, vec!["git", "status"]);
    }

    #[test]
    fn tokenize_quoted_arguments() {
        let cmd = r#"echo "hello world""#;
        let tokens = tokenize_command(cmd).unwrap();
        assert_eq!(tokens, vec!["echo", "hello world"]);
    }

    #[test]
    fn parse_single_command() {
        let script = "git status";
        let commands = parse_shell_commands(script).unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0][0], "git");
    }

    #[test]
    fn parse_chained_commands_with_and() {
        let script = "git status && cargo check";
        let commands = parse_shell_commands(script).unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0][0], "git");
        assert_eq!(commands[1][0], "cargo");
    }

    #[test]
    fn parse_loop_body_commands() {
        let script = "cd crates/codegen/vtcode-core/src/tools/registry && for f in *.rs; do echo \"=== $f ===\"; grep -nE '^(pub )?(struct|enum|fn)' \"$f\" | head -50; done";
        let commands = parse_shell_commands(script).unwrap();

        let expected_directory_change = vec![
            "cd".to_owned(),
            "crates/codegen/vtcode-core/src/tools/registry".to_owned(),
        ];
        assert_eq!(commands.first(), Some(&expected_directory_change));

        let expected_loop_commands = ["echo", "grep", "head"];
        let loop_commands_were_extracted = expected_loop_commands.iter().all(|expected| {
            commands
                .iter()
                .any(|command| command.first().is_some_and(|name| name == *expected))
        });
        assert!(loop_commands_were_extracted);
    }

    #[test]
    fn parse_chained_commands_with_semicolon() {
        let script = "git status; cargo check";
        let commands = parse_shell_commands(script).unwrap();
        assert_eq!(commands.len(), 2);
    }

    #[test]
    fn parse_bash_lc_git_status() {
        let cmd = vec!["bash".to_string(), "-lc".to_string(), "git status".to_string()];
        let commands = parse_bash_lc_commands(&cmd);
        assert!(commands.is_some());
        let commands = commands.unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0][0], "git");
    }

    #[test]
    fn parse_bash_lc_chained() {
        let cmd = vec![
            "bash".to_string(),
            "-lc".to_string(),
            "git status && cargo check".to_string(),
        ];
        let commands = parse_bash_lc_commands(&cmd);
        assert!(commands.is_some());
        let commands = commands.unwrap();
        assert_eq!(commands.len(), 2);
    }

    #[test]
    fn parse_non_bash_command_returns_none() {
        let cmd = vec!["echo".to_string(), "hello".to_string()];
        let commands = parse_bash_lc_commands(&cmd);
        assert!(commands.is_none());
    }

    #[test]
    fn parse_bash_without_lc_returns_none() {
        let cmd = vec!["bash".to_string(), "script.sh".to_string()];
        let commands = parse_bash_lc_commands(&cmd);
        assert!(commands.is_none());
    }

    // Phase 4 tests: Tree-sitter based parsing

    #[test]
    fn parse_complex_pipeline() {
        let script = "cat file.txt | grep -i pattern | sort";
        let commands = parse_shell_commands(script).unwrap();
        assert!(!commands.is_empty());
    }

    #[test]
    fn parse_with_pipes_and_redirects() {
        let script = "ls -la | grep file > output.txt";
        let commands = parse_shell_commands(script).unwrap();
        assert!(!commands.is_empty());
    }

    #[test]
    fn parse_command_substitution_fallback() {
        let script = "echo $(git status)";
        let commands = parse_shell_commands(script).unwrap();
        assert!(!commands.is_empty());
    }

    #[test]
    fn parse_escaped_quotes() {
        let script = r#"echo "hello \"world\"""#;
        let commands = parse_shell_commands(script).unwrap();
        assert!(!commands.is_empty());
    }

    #[test]
    fn parse_tree_sitter_preserves_command_name_with_quoted_args() {
        let script = r#"echo "fish and chips""#;
        let commands = parse_shell_commands_tree_sitter(script).unwrap();
        assert!(!commands.is_empty());
        assert_eq!(commands[0][0], "echo");
    }

    #[test]
    fn parse_tree_sitter_preserves_single_and_ansi_quoted_args() {
        let script = r#"printf '\n' && git diff '--output=out.txt' && printf $'\n'"#;
        let commands = parse_shell_commands_tree_sitter(script).unwrap();
        assert!(
            commands
                .iter()
                .any(|command| command.iter().any(|word| word.contains("--output=out.txt")))
        );
        assert!(commands.iter().any(|command| command.iter().any(|word| word.contains("\\n"))));
    }

    #[test]
    fn dynamic_syntax_allows_literal_escapes_inside_double_quoted_arguments() {
        assert!(!contains_dynamic_shell_syntax(r#"rg -n "\[profile|lto|codegen-units|strip" Cargo.toml"#));
        assert!(!contains_dynamic_shell_syntax(r#"printf "\nTop-level:\n""#));
        assert!(!contains_dynamic_shell_syntax(r#"printf "quoted: \"value\"""#));
        assert!(contains_dynamic_shell_syntax(r#"echo "safe\"$(id)""#));
    }

    #[test]
    fn dynamic_syntax_rejects_unquoted_escapes() {
        assert!(contains_dynamic_shell_syntax(r"rg -n \[profile Cargo.toml"));
    }

    #[test]
    fn output_redirection_guard_rejects_input_and_heredoc_shapes() {
        assert!(has_only_output_redirections("cargo check > build.log 2>&1"));
        assert!(has_only_output_redirections("cargo check | head -40 > build.log"));
        assert!(has_only_output_redirections("cargo check &> build.log"));
        assert!(has_only_output_redirections("cargo check &>> build.log"));
        assert!(!has_only_output_redirections("cargo check < build-input.log"));
        assert!(!has_only_output_redirections("cargo check <<'EOF'\ninput\nEOF"));
        assert!(!has_only_output_redirections("cargo check > $(printf build.log)"));
        assert!(!has_only_output_redirections("cargo check > build.log &"));
        assert!(!has_only_output_redirections("cargo check 2>&-"));
    }

    #[test]
    fn strict_tree_sitter_parser_rejects_incomplete_shell_syntax() {
        assert!(parse_shell_commands_tree_sitter("cargo check &&").is_err());
        assert!(parse_shell_commands_tree_sitter("echo '").is_err());
    }

    #[test]
    fn tree_sitter_issue_337_inputs_are_rejected_before_native_parsing() {
        // `{5𱡀` is the non-adjacent shape from tree-sitter-bash issue #337:
        // the brace need not be byte-adjacent to the 4-byte character.
        // `{\u{e0000}` is the deterministic Linux SIGSEGV reproducer for the
        // locked tree-sitter 0.26.13 and tree-sitter-bash 0.25.1.
        for script in ["{𱡀", "{5𱡀", "{\u{e0000}", "{echo; printf '💩'}"] {
            assert_eq!(
                parse_shell_commands_tree_sitter(script),
                Err("Shell script matches the tree-sitter-bash issue #337 crash pattern".to_string())
            );
            assert_eq!(parse_shell_commands(script), parse_with_basic_tokenization(script));
            assert!(!has_only_output_redirections(script));
        }

        // Quoting would normally let the output-redirection check reach the
        // native parser, so this verifies its issue-pattern preflight directly.
        assert!(!has_only_output_redirections("echo '{' '💩' > build.log"));
    }

    #[test]
    fn tree_sitter_issue_337_guard_requires_brace_before_four_byte_character() {
        assert!(!matches_tree_sitter_bash_issue_337_shape("𱡀{"));
    }

    #[test]
    fn parse_bash_lc_with_pipe() {
        let cmd = vec!["bash".to_string(), "-lc".to_string(), "ls -la | head -5".to_string()];
        let commands = parse_bash_lc_commands(&cmd);
        assert!(commands.is_some());
        let cmds = commands.unwrap();
        assert!(!cmds.is_empty());
    }

    #[test]
    fn parse_dangerous_shell_command() {
        let script = "rm -rf /; echo done";
        let commands = parse_shell_commands(script).unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0][0], "rm");
    }

    #[test]
    fn prewarm_bash_parser_initializes_successfully() {
        prewarm_bash_parser().expect("bash parser should initialize");
    }

    #[test]
    fn dynamic_find_syntax_is_detected_without_rejecting_quoted_globs() {
        assert!(contains_dynamic_find_syntax("find src -maxdepth 0 -exe$''c touch /tmp/VT_BYPASS_POC {} +"));
        assert!(!contains_dynamic_find_syntax("find src -type f -name '*.rs'"));
    }

    #[rstest::rstest]
    #[case("sh", "sh")]
    #[case("'sh'", "sh")]
    #[case("\"sh\"", "sh")]
    #[case("s'h'", "sh")]
    #[case("'s'h", "sh")]
    #[case("''", "")]
    #[case("\"\"", "")]
    #[case("'", "'")]
    #[case("\"", "\"")]
    #[case("'a'\"b\"", "ab")]
    #[case("'$HOME'", "$HOME")]
    #[case("\"$HOME\"", "\"$HOME\"")]
    #[case("'a'b", "ab")]
    #[case("\"s\\\nh\"", "sh")]
    #[case("\"echo \\n\"", "echo \\n")]
    fn strip_literal_shell_quoting_removes_non_expanding_quoting(#[case] token: &str, #[case] expected: &str) {
        assert_eq!(strip_literal_shell_quoting(token).as_ref(), expected);
    }

    #[test]
    fn find_exec_placeholder_is_distinct_from_dynamic_braces_and_expansions() {
        assert!(!contains_dynamic_find_syntax("find src -type f -exec printf '%s\\n' {} \\;"));
        assert!(!contains_dynamic_find_syntax("find src -type f -execdir wc -l {} +"));
        assert!(contains_dynamic_find_syntax("find . -exec {} \\;"));
        assert!(contains_dynamic_find_syntax("find . -exec sh -c {} \\;"));
        assert!(!contains_dynamic_find_syntax("find . -exec printf {} \\;"));
        assert!(contains_dynamic_find_syntax("find src -name {}"));
        assert!(contains_dynamic_find_syntax("find src -name {one,two}"));
        assert!(contains_dynamic_find_syntax("find src -exec printf '%s\\n' ${path} \\;"));
    }

    #[test]
    fn find_exec_bash_command_option_classifies_command_source_as_dynamic() {
        assert!(contains_dynamic_find_syntax("find . -exec bash -c {} \\;"));
    }

    #[test]
    fn find_exec_combined_shell_flags_detect_command_source_without_reclassifying_placeholders() {
        assert!(contains_dynamic_find_syntax("find . -exec sh -ec {} \\;"));
        assert!(!contains_dynamic_find_syntax("find . -exec sh -ec 'printf %s' {} \\;"));
        assert!(!contains_dynamic_find_syntax("find . -exec sh -e printf {} \\;"));
    }

    #[test]
    fn find_exec_quoted_literal_tokens_stay_dynamic() {
        // Extraction keeps the quote characters, so a quoted interpreter,
        // command option and placeholder must still be recognised after the
        // non-expanding quoting is stripped.
        assert!(contains_dynamic_find_syntax("find . -exec 'sh' -c '{}' \\;"));
        assert!(contains_dynamic_find_syntax("find . -exec \"bash\" -c \"{}\" \\;"));
        assert!(contains_dynamic_find_syntax("find . -exec 'sh' '-c' {} \\;"));
        // Partly quoted names and options unquote to the same runtime token.
        assert!(contains_dynamic_find_syntax("find . -exec s'h' -c '{}' \\;"));
        assert!(contains_dynamic_find_syntax("find . -exec sh '-c' '{}' \\;"));
        // A quoted env split-string option still splits its argument.
        assert!(contains_dynamic_find_syntax("find . -exec 'env' '-S' 'sh -c {}' \\;"));
        // A quoted `-exec`/`-execdir` is still the find expression.
        assert!(contains_dynamic_find_syntax("find . '-exec' sh -c '{}' +"));
        assert!(contains_dynamic_find_syntax("find . \"-execdir\" sh -c '{}' +"));
        assert!(contains_dynamic_find_syntax("find . '-exec' sh -c '{}' ';'"));
        // Quoting that performs expansion stays rejected, quoted or not.
        assert!(contains_dynamic_find_syntax("find . -exec sh -c \"$1\" {} \\;"));
    }

    #[test]
    fn find_exec_recognizes_supported_shell_interpreters() {
        for shell in ["sh", "bash", "dash", "zsh", "ksh", "ksh93", "ash", "mksh"] {
            let script = format!("find . -exec {shell} -c {{}} \\;");
            assert!(contains_dynamic_find_syntax(&script), "expected dynamic shell source: {script}");
        }
    }

    #[test]
    fn find_exec_wrapped_shell_interpreter_detects_command_source() {
        for script in [
            "find . -exec timeout 5 bash -c {} \\;",
            "find . -exec nice -n 5 sh -c {} \\;",
            "find . -exec nohup bash -c {} \\;",
            "find . -exec stdbuf -oL bash -c {} \\;",
            "find . -exec xargs bash -c {} \\;",
        ] {
            assert!(contains_dynamic_find_syntax(script), "expected dynamic shell source: {script}");
        }

        // A literal interpreter-name argument is not an executable position
        // unless it is followed by a shell command option.
        assert!(!contains_dynamic_find_syntax("find src -type f -exec cp bash {} \\;"));
        assert!(!contains_dynamic_find_syntax("find src -type f -exec grep -e bash {} \\;"));
    }

    #[test]
    fn find_exec_interpreter_behind_any_launcher_stays_dynamic() {
        // Launchers cannot be enumerated, so an interpreter in an argument
        // position is treated as executable. Over-approximating costs an extra
        // approval; under-approximating would run filename-derived source.
        for script in [
            "find . -exec sudo sh -c '{}' \\;",
            "find . -exec chroot / sh -c '{}' \\;",
            "find . -exec setsid bash -c '{}' \\;",
            "find . -exec ionice sh -c '{}' \\;",
            "find . -exec busybox sh -c '{}' \\;",
            "find . -exec printf '%s\\n' sh -c '{}' \\;",
        ] {
            assert!(contains_dynamic_find_syntax(script), "expected dynamic shell source: {script}");
        }
    }

    #[test]
    fn find_exec_env_wrapper_detects_nested_shell_command_source() {
        assert!(contains_dynamic_find_syntax("find . -exec env bash -c {} \\;"));
        assert!(contains_dynamic_find_syntax("find . -exec env timeout 5 sh -c '{}' \\;"));
        assert!(contains_dynamic_find_syntax("find . -exec env -i -- bash -ec {} \\;"));
        assert!(contains_dynamic_find_syntax("find . -exec env -u HOME -- bash -c {} \\;"));
        assert!(contains_dynamic_find_syntax("find . -exec env --split-string 'bash -c {}' \\;"));
        assert!(contains_dynamic_find_syntax("find . -exec timeout 5 env -S 'sh -c {}' \\;"));
        assert!(contains_dynamic_find_syntax("find . -exec env env -S 'sh -c {}' ';'"));
        assert!(contains_dynamic_find_syntax("find . -exec timeout 5 /usr/bin/env -S 'sh -c {}' ';'"));
        assert!(!contains_dynamic_find_syntax("find . -exec env bash -c 'printf %s' {} \\;"));
    }

    #[test]
    fn find_exec_predicate_operand_is_not_an_action() {
        // `-name` consumes the quoted `-exec` as its pattern, so the real
        // action that follows still starts an exec.
        assert!(contains_dynamic_find_syntax("find . -name '-exec' -exec {} \\;"));
        assert!(contains_dynamic_find_syntax("find . -type f -exec {} \\;"));
        // `-fprintf` consumes a file name and a format, so the quoted format
        // cannot start a fictitious action.
        assert!(contains_dynamic_find_syntax("find . -fprintf output '-exec' -exec '{}' \\;"));
        assert!(contains_dynamic_find_syntax("find . -fprint '-exec' -exec '{}' \\;"));
        // Only a whole-argument `{}` enables the `{} +` terminator, so an
        // `x{}` argument must not end the action before a later one starts.
        assert!(contains_dynamic_find_syntax("find . -exec printf '%s' 'x{}' + -fprintf ';' -exec sh -c '{}' ';'"));
        // Ordinary patterns stay static.
        assert!(!contains_dynamic_find_syntax("find . -name '*.rs' -exec printf {} \\;"));
    }

    #[test]
    fn find_exec_terminator_ends_the_action_in_every_context() {
        // The quoted `';'` reaches `find` as `;`, so it ends the first action
        // even though it follows `sh -c`.
        assert!(contains_dynamic_find_syntax("find . -exec sh -c ';' , -exec {} \\;"));
        assert!(contains_dynamic_find_syntax("find . -exec env -u ';' -exec {} \\;"));
    }

    #[test]
    fn find_exec_env_program_placeholder_stays_dynamic() {
        // Quoting suppresses brace expansion, but `find` still substitutes a
        // path into the program position, which `env` then executes.
        assert!(contains_dynamic_find_syntax("find . -exec env '{}' \\;"));
        assert!(contains_dynamic_find_syntax("find . -exec env -- '{}' \\;"));
        assert!(contains_dynamic_find_syntax("find . -exec env \"{}\" \\;"));
    }

    #[test]
    fn find_exec_quoted_env_assignments_keep_raw_dynamic_syntax() {
        // The literal value decides the env context, but an expansion in the
        // raw token is dynamic even when quoting hides it from the shape check.
        assert!(contains_dynamic_find_syntax("find . -exec 'env' \"X=$(id)\" printf '{}' \\;"));
        assert!(contains_dynamic_find_syntax("find . -exec env \"X=`id`\" printf '{}' \\;"));
        // A wholly literal assignment stays static.
        assert!(!contains_dynamic_find_syntax("find . -exec env X='value' printf '{}' \\;"));
    }

    #[test]
    fn find_exec_env_option_operands_keep_raw_dynamic_syntax() {
        assert!(contains_dynamic_find_syntax("find . -exec env -u \"$(id)\" printf '{}' \\;"));
        assert!(contains_dynamic_find_syntax("find . -exec env -C \"$HOME\" printf '{}' \\;"));
        assert!(!contains_dynamic_find_syntax("find . -exec env -u HOME printf '{}' \\;"));
    }

    #[test]
    fn find_exec_placeholder_hidden_by_mixed_quoting_stays_dynamic() {
        // The placeholder is split across a double-quoted segment that ends in
        // an escaped space and a single-quoted segment, so the raw token has
        // no `{}` and Bash still forms one after quote removal. The script is
        // balanced, so the classifier runs instead of the raw fallback.
        let script = "find . '-exec' sh -c \"echo \\n \"'{''}' \\;";
        assert!(parse_shell_commands_tree_sitter(script).is_ok(), "script must parse: {script}");
        assert!(contains_dynamic_find_syntax(script));
        assert!(!contains_dynamic_find_syntax("find . -exec sh -c 'printf %s' {} \\;"));
    }

    #[test]
    fn find_exec_action_token_in_an_operand_position_does_not_start_an_exec() {
        // `env -u '-exec'` consumes the quoted token as an operand, so the
        // following `X=value` is still an env assignment and the program
        // position it guards stays dynamic.
        assert!(contains_dynamic_find_syntax("find . -exec env -u '-exec' X=value {} \\;"));
        // A real action after a genuine terminator still starts an exec.
        assert!(contains_dynamic_find_syntax("find . -exec printf %s \\; '-exec' sh -c {} \\;"));
    }

    #[test]
    fn find_exec_quoted_terminator_does_not_end_the_exec_expression() {
        // A quoted `'\;'` reaches `find` as the two characters `\;` and does
        // not terminate, so the following `sh -c '{}'` stays dynamic.
        assert!(contains_dynamic_find_syntax("find . -exec sudo -p '\\;' sh -c '{}' +"));
        assert!(contains_dynamic_find_syntax("find . -exec sudo -p \"\\;\" sh -c '{}' +"));
        assert!(contains_dynamic_find_syntax("find . -exec env -u x '\\;' sh -c '{}' +"));
        // A `+` only ends the expression when the placeholder precedes it.
        assert!(contains_dynamic_find_syntax("find . -exec sudo -p + sh -c '{}' ';'"));
        // A genuine terminator still ends the expression.
        assert!(!contains_dynamic_find_syntax("find . -exec printf %s {} \\;"));
        assert!(!contains_dynamic_find_syntax("find . -exec printf %s {} '+'"));
    }

    #[test]
    fn find_exec_double_quoted_line_continuation_stays_dynamic() {
        // Bash drops a double-quoted backslash-newline pair, forming `sh`.
        assert!(contains_dynamic_find_syntax("find . -exec \"s\\\nh\" -c {} \\;"));
        // A literal backslash-then-`n` is not a continuation.
        assert_eq!(strip_literal_shell_quoting("\"echo \\n\"").as_ref(), "echo \\n");
    }

    #[test]
    fn find_exec_env_end_of_options_preserves_following_find_expressions() {
        assert!(contains_dynamic_find_syntax("find . -exec env -- \\; -exec bash -c {} \\;"));
        assert!(!contains_dynamic_find_syntax("find . -exec env -- \\; -exec printf {} \\;"));
    }

    proptest! {
        #[test]
        fn literal_placeholder_remains_static_across_find_exec_commands(
            root in "[a-zA-Z0-9_./-]{1,40}",
            utility in "[a-zA-Z][a-zA-Z0-9_-]{0,20}",
            argument in "[a-zA-Z0-9_./:-]{1,40}",
            exec_option in prop_oneof![Just("-exec"), Just("-execdir")],
            terminator in prop_oneof![Just("+"), Just("\\;")],
        ) {
            // A shell interpreter or `env` in the program position makes the
            // following tokens command source or options, so those cases are
            // correctly dynamic and belong to the dynamic regressions instead.
            prop_assume!(!is_shell_interpreter(&utility));
            prop_assume!(!is_shell_command_option(&argument));
            prop_assume!(utility != "env");
            // An `env` argument starts the env command grammar, so the
            // following `{}` is a program position and correctly dynamic.
            prop_assume!(argument != "env");

            let script = format!("find {root} -type f {exec_option} {utility} {argument} {{}} {terminator}");

            prop_assert!(!contains_dynamic_find_syntax(&script), "unexpectedly dynamic: {script}");
        }
    }
}

// === Injection detection (moved from tools::validation::commands) ===

use anyhow::{Result, bail};

/// Quote state for shell segment splitting.
#[derive(Clone, Copy, Eq, PartialEq)]
enum QuoteState {
    None,
    Single,
    Double,
}

/// Split a shell command into segments on unquoted `|` and `&` boundaries,
/// while detecting injection patterns (`;`, backticks, `$()`, newlines).
pub(crate) fn split_shell_segments(command: &str) -> Result<Vec<String>> {
    let mut segments = Vec::new();
    let mut state = QuoteState::None;
    let mut escaped = false;
    let mut segment_start = 0usize;
    let mut chars = command.char_indices().peekable();

    while let Some((idx, ch)) = chars.next() {
        match state {
            QuoteState::Single => {
                if ch == '\'' {
                    state = QuoteState::None;
                }
            }
            QuoteState::Double => {
                if escaped {
                    escaped = false;
                    continue;
                }

                match ch {
                    '\\' => escaped = true,
                    '"' => state = QuoteState::None,
                    '`' => bail!("Command injection pattern detected"),
                    '$' if matches!(chars.peek(), Some((_, '('))) => {
                        bail!("Command injection pattern detected");
                    }
                    _ => {}
                }
            }
            QuoteState::None => {
                if escaped {
                    escaped = false;
                    continue;
                }

                match ch {
                    '\\' => escaped = true,
                    '\'' => state = QuoteState::Single,
                    '"' => state = QuoteState::Double,
                    '`' => bail!("Command injection pattern detected"),
                    '$' if matches!(chars.peek(), Some((_, '('))) => {
                        bail!("Command injection pattern detected");
                    }
                    ';' => bail!("Unquoted command chaining detected"),
                    '\n' => bail!("Command injection pattern detected"),
                    '|' | '&' => {
                        push_segment(command, segment_start, idx, &mut segments);
                        segment_start = idx + ch.len_utf8();
                        if let Some((next_idx, next_ch)) = chars.peek().copied()
                            && next_ch == ch
                        {
                            let _next = chars.next();
                            segment_start = next_idx + next_ch.len_utf8();
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    push_segment(command, segment_start, command.len(), &mut segments);
    Ok(segments)
}

fn push_segment(command: &str, start: usize, end: usize, segments: &mut Vec<String>) {
    let segment = command[start..end].trim();
    if !segment.is_empty() {
        segments.push(segment.to_string());
    }
}

/// Check for additional dangerous patterns not covered by the central dangerous-command detector.
pub(crate) fn additional_dangerous_pattern(segment: &str) -> Option<&'static str> {
    let segment_lower = segment.to_ascii_lowercase();
    if segment_lower.starts_with(":(){:|:&};:") {
        return Some(":(){:|:&};:");
    }

    let tokens =
        shell_words::split(segment).unwrap_or_else(|_| segment.split_whitespace().map(ToString::to_string).collect());
    let first = tokens.first()?;
    let command_name = base_command_name(strip_wrapping_quotes(first)).to_ascii_lowercase();

    match command_name.as_str() {
        "rmdir" => Some("rmdir"),
        "wget" => Some("wget"),
        "curl" => Some("curl"),
        "chmod" if tokens.iter().skip(1).any(|arg| strip_wrapping_quotes(arg).starts_with("777")) => Some("chmod 777"),
        "chown"
            if tokens.iter().skip(1).any(|arg| {
                let arg = strip_wrapping_quotes(arg).to_ascii_lowercase();
                arg == "root" || arg.starts_with("root:")
            }) =>
        {
            Some("chown root")
        }
        _ => None,
    }
}

fn strip_wrapping_quotes(token: &str) -> &str {
    token
        .strip_prefix('\'')
        .and_then(|token| token.strip_suffix('\''))
        .or_else(|| token.strip_prefix('"').and_then(|token| token.strip_suffix('"')))
        .unwrap_or(token)
}

fn base_command_name(command: &str) -> &str {
    std::path::Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
}
