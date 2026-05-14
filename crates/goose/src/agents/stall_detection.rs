use crate::conversation::message::{Message, MessageContent};
use rmcp::model::Role;

pub const STALL_HINT: &str = "Note: No forward progress since your last response — \
no file write or edit has happened since the previous user turn. Before continuing, \
summarize what is blocking you, the design choice you are facing, or what you need \
from me, then proceed.";

const STATE_CHANGE_TOOLS: &[&str] = &[
    "write",
    "edit",
    "text_editor",
    "todo_write",
    "developer__write",
    "developer__edit",
    "developer__text_editor",
    "developer__todo_write",
];

pub fn is_bare_nudge(text: &str) -> bool {
    let trimmed = text
        .trim()
        .trim_end_matches('.')
        .trim()
        .to_ascii_lowercase();
    matches!(
        trimmed.as_str(),
        "continue" | "yes" | "y" | "ok" | "okay" | "do it" | "go" | "proceed" | "next" | ""
    )
}

/// Walks newest-first through messages, stopping at the most recent user
/// message that is *not* the one we're evaluating. Returns true if no
/// state-change tool request appeared between then and now.
pub fn no_state_change_since_prior_user(messages: &[Message]) -> bool {
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
            if let MessageContent::ToolRequest(tr) = content {
                if let Ok(tc) = &tr.tool_call {
                    if STATE_CHANGE_TOOLS.contains(&tc.name.as_ref()) {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// Cooldown check: looks at the user message *before* the current nudge
/// (i.e. second-most-recent user message in history) and returns true if it
/// begins with the stall-hint prefix. Used to avoid firing the hint twice
/// in a row when the user types another bare nudge.
pub fn previous_user_message_is_stall_hint(messages: &[Message]) -> bool {
    let prefix = stall_hint_prefix();
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

fn stall_hint_prefix() -> &'static str {
    "Note: No forward progress"
}

/// Returns true if the current user message is a bare nudge, no state-change
/// tool ran since the previous user message, and we did not already inject
/// the stall hint last turn.
pub fn should_inject_stall_hint(current_user_text: &str, messages: &[Message]) -> bool {
    if !is_bare_nudge(current_user_text) {
        return false;
    }
    if previous_user_message_is_stall_hint(messages) {
        return false;
    }
    no_state_change_since_prior_user(messages)
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
            ".",
            "",
            "  yes  ",
        ] {
            assert!(is_bare_nudge(t), "should treat {t:?} as bare nudge");
        }
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
    fn no_state_change_returns_true_when_no_writes_between_user_turns() {
        let msgs = vec![
            user("first ask"),
            assistant_tool("read"),
            assistant_tool("read"),
            user("continue"),
        ];
        assert!(no_state_change_since_prior_user(&msgs));
    }

    #[test]
    fn no_state_change_returns_false_when_write_appears() {
        let msgs = vec![
            user("first ask"),
            assistant_tool("read"),
            assistant_tool("write"),
            user("continue"),
        ];
        assert!(!no_state_change_since_prior_user(&msgs));
    }

    #[test]
    fn no_state_change_recognizes_developer_prefixed_tools() {
        let msgs = vec![
            user("first ask"),
            assistant_tool("developer__text_editor"),
            user("continue"),
        ];
        assert!(!no_state_change_since_prior_user(&msgs));
    }

    #[test]
    fn no_state_change_returns_true_with_only_one_user_turn_and_no_writes() {
        let msgs = vec![user("only ask"), assistant_tool("read")];
        assert!(no_state_change_since_prior_user(&msgs));
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
    fn should_inject_fires_on_bare_nudge_after_stall() {
        let msgs = vec![
            user("debug this"),
            assistant_tool("shell"),
            assistant_tool("shell"),
            user("continue"),
        ];
        assert!(should_inject_stall_hint("continue", &msgs));
    }

    #[test]
    fn should_inject_skips_when_substantive_message() {
        let msgs = vec![
            user("debug this"),
            assistant_tool("shell"),
            user("look at line 42"),
        ];
        assert!(!should_inject_stall_hint("look at line 42", &msgs));
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
            assistant_tool("shell"),
            user(STALL_HINT),
            Message::assistant().with_text("summary"),
            user("yes"),
        ];
        assert!(!should_inject_stall_hint("yes", &msgs));
    }
}
