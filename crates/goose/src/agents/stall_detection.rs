use crate::conversation::message::{Message, MessageContent};
use once_cell::sync::Lazy;
use regex::Regex;
use rmcp::model::Role;

pub const STALL_HINT: &str = "Note: No tool call has happened since the previous user turn. \
If you are blocked, say what you need. Otherwise proceed.";

const STALL_HINT_PREFIX: &str = "Note: No tool call";

pub const PLAN_HINT: &str = "Note: This looks like a multi-phase implementation. \
Before making changes, call `todo_write` to externalize the plan — your todo content \
is auto-injected on every turn, so you don't need to re-state it in `thinking` blocks. \
Update items in place as you go.";

const PLAN_HINT_PREFIX: &str = "Note: This looks like a multi-phase";

// Capitalized .md filenames are a high-signal convention for design docs.
static PLAN_FILENAME_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b(PLAN|BLUEPRINT|DESIGN|SPEC|ROADMAP)\.md\b").expect("static regex")
});

// Action verb + the word "plan" within 30 chars. Case-insensitive on the verb.
static PLAN_ACTION_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(implement|execute|follow|build out|work through|go through)\b.{0,30}\bplan\b",
    )
    .expect("static regex")
});

const TODO_WRITE_TOOL_NAMES: &[&str] = &[
    "todo_write",
    "developer__todo_write",
    "platform__todo_write",
];

pub fn is_bare_nudge(text: &str) -> bool {
    let trimmed = text
        .trim()
        .trim_end_matches('.')
        .trim()
        .to_ascii_lowercase();
    matches!(
        trimmed.as_str(),
        "continue" | "yes" | "y" | "ok" | "okay" | "do it" | "go" | "proceed" | "next"
    )
}

/// Walks newest-first through messages, stopping at the most recent user
/// message that is *not* the one we're evaluating. Returns true if no
/// tool request of any kind appeared between then and now. Reads count as
/// progress — investigation-heavy workflows (PR review, debugging) should
/// not trip the stall detector.
pub fn no_tool_request_since_prior_user(messages: &[Message]) -> bool {
    let mut user_seen = 0u32;
    for msg in messages.iter().rev() {
        if msg.role == Role::User {
            user_seen += 1;
            if user_seen >= 2 {
                return true;
            }
            continue;
        }
        if msg.role != Role::Assistant {
            continue;
        }
        for content in &msg.content {
            if matches!(content, MessageContent::ToolRequest(_)) {
                return false;
            }
        }
    }
    true
}

/// Cooldown check: looks at the user message *before* the current nudge
/// (i.e. second-most-recent user message in history) and returns true if its
/// first non-empty text content begins with `prefix`.
fn previous_user_message_starts_with(messages: &[Message], prefix: &str) -> bool {
    let mut user_count = 0u32;
    for msg in messages.iter().rev() {
        if msg.role != Role::User {
            continue;
        }
        let mut text_for_msg: Option<&str> = None;
        for content in &msg.content {
            if let MessageContent::Text(t) = content {
                let trimmed = t.text.trim();
                if !trimmed.is_empty() {
                    text_for_msg = Some(trimmed);
                    break;
                }
            }
        }
        if text_for_msg.is_none() {
            continue;
        }
        user_count += 1;
        if user_count == 2 {
            return text_for_msg.unwrap().starts_with(prefix);
        }
    }
    false
}

/// True if we already injected the stall hint on the previous user turn.
/// Suppresses re-firing on consecutive bare nudges.
pub fn previous_user_message_is_stall_hint(messages: &[Message]) -> bool {
    previous_user_message_starts_with(messages, STALL_HINT_PREFIX)
}

/// True if we already injected the plan hint on the previous user turn.
pub fn previous_user_message_is_plan_hint(messages: &[Message]) -> bool {
    previous_user_message_starts_with(messages, PLAN_HINT_PREFIX)
}

/// Detects plan-shaped user requests: either a capitalized `.md` filename
/// (PLAN.md / BLUEPRINT.md / etc.) or an action verb paired with the word
/// "plan" (e.g. "implement this plan", "follow the plan").
pub fn is_plan_implementation_request(text: &str) -> bool {
    PLAN_FILENAME_RE.is_match(text) || PLAN_ACTION_RE.is_match(text)
}

/// True if the most recent assistant turn (if any) issued a `todo_write` tool
/// request. Walks newest-first; stops at the first assistant message it
/// encounters — earlier todo_write calls in older turns don't suppress.
pub fn most_recent_assistant_called_todo_write(messages: &[Message]) -> bool {
    for msg in messages.iter().rev() {
        if msg.role != Role::Assistant {
            continue;
        }
        for content in &msg.content {
            if let MessageContent::ToolRequest(tr) = content {
                if let Ok(tc) = &tr.tool_call {
                    if TODO_WRITE_TOOL_NAMES.contains(&tc.name.as_ref()) {
                        return true;
                    }
                }
            }
        }
        return false;
    }
    false
}

/// Returns true if the current user message is a bare nudge, no tool request
/// of any kind happened since the previous user message, and we did not
/// already inject the stall hint last turn.
pub fn should_inject_stall_hint(current_user_text: &str, messages: &[Message]) -> bool {
    if !is_bare_nudge(current_user_text) {
        return false;
    }
    if previous_user_message_is_stall_hint(messages) {
        return false;
    }
    no_tool_request_since_prior_user(messages)
}

/// Returns true if the current user message looks like a plan-implementation
/// request, the most recent assistant turn didn't already call `todo_write`,
/// and we didn't already nag on the prior user turn.
pub fn should_inject_plan_hint(current_user_text: &str, messages: &[Message]) -> bool {
    if !is_plan_implementation_request(current_user_text) {
        return false;
    }
    if previous_user_message_is_plan_hint(messages) {
        return false;
    }
    if most_recent_assistant_called_todo_write(messages) {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::CallToolRequestParams;
    use serde_json::json;

    fn user(text: &str) -> Message {
        Message::user().with_text(text)
    }

    fn assistant_tool(name: &'static str) -> Message {
        let tool_call = Ok(CallToolRequestParams::new(name)
            .with_arguments(json!({}).as_object().cloned().unwrap_or_default()));
        Message::assistant().with_tool_request("id-x", tool_call)
    }

    #[test]
    fn bare_nudge_recognizes_common_forms() {
        for t in [
            "continue",
            "Continue",
            "Continue.",
            "yes",
            "Yes",
            "y",
            "ok",
            "Okay",
            "do it",
            "Do it.",
            "go",
            "proceed",
            "next",
            "  yes  ",
        ] {
            assert!(is_bare_nudge(t), "should treat {t:?} as bare nudge");
        }
    }

    #[test]
    fn bare_nudge_rejects_empty_input() {
        assert!(!is_bare_nudge(""));
        assert!(!is_bare_nudge("   "));
    }

    #[test]
    fn bare_nudge_rejects_substantive_messages() {
        for t in [
            "please look at line 42",
            "yes, but also check foo",
            "continue working on the parser",
            "let's go with option B",
            "what about option C?",
        ] {
            assert!(!is_bare_nudge(t), "should not treat {t:?} as bare nudge");
        }
    }

    #[test]
    fn no_tool_request_returns_true_when_only_text_between_user_turns() {
        let msgs = vec![
            user("first ask"),
            Message::assistant().with_text("thinking out loud"),
            Message::assistant().with_text("still thinking"),
            user("continue"),
        ];
        assert!(no_tool_request_since_prior_user(&msgs));
    }

    #[test]
    fn no_tool_request_returns_false_when_read_appears() {
        let msgs = vec![user("first ask"), assistant_tool("read"), user("continue")];
        assert!(!no_tool_request_since_prior_user(&msgs));
    }

    #[test]
    fn no_tool_request_returns_false_when_shell_appears() {
        let msgs = vec![user("first ask"), assistant_tool("shell"), user("continue")];
        assert!(!no_tool_request_since_prior_user(&msgs));
    }

    #[test]
    fn no_tool_request_returns_false_when_write_appears() {
        let msgs = vec![user("first ask"), assistant_tool("write"), user("continue")];
        assert!(!no_tool_request_since_prior_user(&msgs));
    }

    #[test]
    fn no_tool_request_returns_true_with_only_one_user_turn_and_no_tools() {
        let msgs = vec![user("only ask"), Message::assistant().with_text("ok")];
        assert!(no_tool_request_since_prior_user(&msgs));
    }

    #[test]
    fn cooldown_blocks_consecutive_firing() {
        // Convo at check time: user(STALL_HINT) is the previous user message,
        // user("continue") is the current nudge being evaluated.
        let msgs = vec![
            user("debug"),
            Message::assistant().with_text("looking..."),
            user(STALL_HINT),
            Message::assistant().with_text("ok let me summarize"),
            user("continue"),
        ];
        assert!(previous_user_message_is_stall_hint(&msgs));
    }

    #[test]
    fn cooldown_does_not_match_substantive_prior_user_text() {
        // Previous user message is "ok try option B"; current is "continue".
        let msgs = vec![
            user(STALL_HINT),
            Message::assistant().with_text("summary..."),
            user("ok try option B"),
            Message::assistant().with_text("trying..."),
            user("continue"),
        ];
        assert!(!previous_user_message_is_stall_hint(&msgs));
    }

    #[test]
    fn cooldown_returns_false_when_history_has_only_current_user_message() {
        let msgs = vec![user("continue")];
        assert!(!previous_user_message_is_stall_hint(&msgs));
    }

    #[test]
    fn should_inject_fires_on_bare_nudge_after_text_only_turn() {
        let msgs = vec![
            user("debug this"),
            Message::assistant().with_text("let me think about this"),
            Message::assistant().with_text("hmm"),
            user("continue"),
        ];
        assert!(should_inject_stall_hint("continue", &msgs));
    }

    #[test]
    fn should_inject_skips_when_substantive_message() {
        let msgs = vec![
            user("debug this"),
            Message::assistant().with_text("thinking"),
            user("look at line 42"),
        ];
        assert!(!should_inject_stall_hint("look at line 42", &msgs));
    }

    #[test]
    fn should_inject_skips_when_any_tool_ran() {
        let msgs = vec![
            user("review this PR"),
            assistant_tool("shell"),
            user("continue"),
        ];
        assert!(!should_inject_stall_hint("continue", &msgs));
    }

    #[test]
    fn should_inject_skips_when_write_happened() {
        let msgs = vec![
            user("fix the bug"),
            assistant_tool("write"),
            user("continue"),
        ];
        assert!(!should_inject_stall_hint("continue", &msgs));
    }

    #[test]
    fn should_inject_skips_when_cooldown_active() {
        let msgs = vec![
            user("debug"),
            Message::assistant().with_text("hmm"),
            user(STALL_HINT),
            Message::assistant().with_text("summary"),
            user("yes"),
        ];
        assert!(!should_inject_stall_hint("yes", &msgs));
    }

    // ── Plan-hint tests ──────────────────────────────────────────

    #[test]
    fn plan_request_recognizes_capitalized_md_files() {
        for t in [
            "Help me implement this plan -  PLUGIN_SYSTEM_PLAN.md",
            "look at PLAN.md",
            "follow BLUEPRINT.md",
            "check DESIGN.md",
            "implement the SPEC.md please",
            "ROADMAP.md is the source of truth",
        ] {
            assert!(
                is_plan_implementation_request(t),
                "should detect plan-shaped request in {t:?}"
            );
        }
    }

    #[test]
    fn plan_request_recognizes_action_verb_phrases() {
        for t in [
            "implement this plan",
            "Implement the plan",
            "execute the plan now",
            "follow our deployment plan step by step",
            "go through the plan one item at a time",
            "work through the plan with me",
        ] {
            assert!(
                is_plan_implementation_request(t),
                "should detect plan-shaped request in {t:?}"
            );
        }
    }

    #[test]
    fn plan_request_rejects_non_plan_messages() {
        for t in [
            "what's our deployment plan?",
            "can you read plan.md", // lowercase .md filename, no action verb
            "explain how this works",
            "the plan we discussed",     // no action verb
            "review the planning notes", // no bare "plan"
        ] {
            assert!(
                !is_plan_implementation_request(t),
                "should NOT detect plan-shaped request in {t:?}"
            );
        }
    }

    fn assistant_tool_with_name(name: &'static str, id: &str) -> Message {
        let tool_call = Ok(CallToolRequestParams::new(name)
            .with_arguments(json!({}).as_object().cloned().unwrap_or_default()));
        Message::assistant().with_tool_request(id, tool_call)
    }

    #[test]
    fn most_recent_assistant_called_todo_write_detects_bare_name() {
        let msgs = vec![
            user("implement the plan"),
            assistant_tool_with_name("todo_write", "tw-1"),
        ];
        assert!(most_recent_assistant_called_todo_write(&msgs));
    }

    #[test]
    fn most_recent_assistant_called_todo_write_detects_prefixed_name() {
        let msgs = vec![
            user("implement the plan"),
            assistant_tool_with_name("developer__todo_write", "tw-1"),
        ];
        assert!(most_recent_assistant_called_todo_write(&msgs));
    }

    #[test]
    fn most_recent_assistant_called_todo_write_returns_false_for_recent_other_tool() {
        let msgs = vec![
            user("implement the plan"),
            assistant_tool_with_name("read", "r-1"),
        ];
        assert!(!most_recent_assistant_called_todo_write(&msgs));
    }

    #[test]
    fn most_recent_assistant_called_todo_write_only_checks_most_recent() {
        let msgs = vec![
            user("first"),
            assistant_tool_with_name("todo_write", "tw-1"), // older
            user("second"),
            assistant_tool_with_name("read", "r-1"), // most recent assistant turn
        ];
        assert!(!most_recent_assistant_called_todo_write(&msgs));
    }

    #[test]
    fn should_inject_plan_fires_on_plan_file_with_no_todo_write() {
        let msgs = vec![user("Help me implement this plan - PLUGIN_SYSTEM_PLAN.md")];
        assert!(should_inject_plan_hint(
            "Help me implement this plan - PLUGIN_SYSTEM_PLAN.md",
            &msgs
        ));
    }

    #[test]
    fn should_inject_plan_skips_when_todo_write_already_called() {
        let msgs = vec![
            user("implement the plan"),
            assistant_tool_with_name("todo_write", "tw-1"),
            user("implement the plan now"),
        ];
        assert!(!should_inject_plan_hint("implement the plan now", &msgs));
    }

    #[test]
    fn should_inject_plan_skips_on_cooldown() {
        let msgs = vec![
            user("implement the plan"),
            Message::assistant().with_text("ok"),
            user(PLAN_HINT),
            Message::assistant().with_text("understood"),
            user("execute the plan"),
        ];
        assert!(!should_inject_plan_hint("execute the plan", &msgs));
    }

    #[test]
    fn should_inject_plan_skips_on_non_plan_input() {
        let msgs = vec![user("look at line 42")];
        assert!(!should_inject_plan_hint("look at line 42", &msgs));
    }
}
