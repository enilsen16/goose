use crate::config::GooseMode;
use crate::conversation::message::{Message, MessageContent, ToolRequest};
use crate::tool_inspection::{InspectionAction, InspectionResult, ToolInspector};
use anyhow::Result;
use async_trait::async_trait;
use once_cell::sync::Lazy;
use regex::Regex;
use rmcp::model::{CallToolRequestParams, Role};
use serde_json::Value;
use std::sync::Mutex;

pub const FINDING_ID_REPEATED_CALLS: &str = "REP-001";
pub const FINDING_ID_REPEATED_ERROR: &str = "REP-002";
pub const FINDING_ID_REPEATED_PATH: &str = "REP-003";
const MAX_CONSECUTIVE_ERROR_FINGERPRINTS: u32 = 3;
const DEFAULT_MAX_SAME_PATH_READS: u32 = 5;

// Tool-output error fingerprints used by REP-002. Each captures the structural
// id of the failure (rustc error code, tsc code, failing test name) along with
// up to 80 chars of post-code context, so two unrelated errors with the same
// code don't collapse but the same error across re-runs does.
static RUSTC_ERROR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"error\[(E\d{4})\][^\n]{0,80}").expect("static regex"));
static TSC_ERROR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"error TS(\d+):[^\n]{0,80}").expect("static regex"));
static PYTEST_FAIL_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"FAILED tests/\S+").expect("static regex"));
static CARGO_TEST_FAIL_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"test (\S+) \.\.\. FAILED").expect("static regex"));

/// Extract a stable fingerprint from a tool's error output. For shell-family
/// tools we look for compiler/test-runner identifiers; everything else falls
/// back to the legacy 100-char prefix so unknown tools keep their old behavior.
fn fingerprint_error(tool_name: &str, error_text: &str) -> String {
    if tool_name.contains("shell") {
        for re in [
            &*RUSTC_ERROR_RE,
            &*TSC_ERROR_RE,
            &*CARGO_TEST_FAIL_RE,
            &*PYTEST_FAIL_RE,
        ] {
            if let Some(m) = re.find(error_text) {
                return m.as_str().to_string();
            }
        }
    }
    error_text.chars().take(100).collect()
}

fn extract_path_arg(args: Option<&serde_json::Map<String, Value>>) -> Option<&str> {
    args?.get("path")?.as_str()
}

/// Counts reads of `path` by `tool_name` since the most recent write or edit
/// to that path, scanning newest-first for early termination.
fn path_read_count_in_history(messages: &[Message], tool_name: &str, path: &str) -> u32 {
    const WRITE_TOOLS: &[&str] = &["write", "edit"];
    let mut count = 0u32;
    'msg: for msg in messages.iter().rev() {
        if msg.role != Role::Assistant {
            continue;
        }
        for content in &msg.content {
            let MessageContent::ToolRequest(tr) = content else {
                continue;
            };
            let Ok(tc) = &tr.tool_call else { continue };
            let Some(call_path) = extract_path_arg(tc.arguments.as_ref()) else {
                continue;
            };
            if call_path != path {
                continue;
            }
            if tc.name.as_ref() == tool_name {
                count += 1;
            } else if WRITE_TOOLS.contains(&tc.name.as_ref()) {
                break 'msg;
            }
        }
    }
    count
}

#[derive(Debug, Clone)]
struct InternalToolCall {
    name: String,
    parameters: Value,
}

impl InternalToolCall {
    fn matches(&self, other: &InternalToolCall) -> bool {
        self.name == other.name && self.parameters == other.parameters
    }

    fn from_tool_call(tool_call: &CallToolRequestParams) -> Self {
        let name = tool_call.name.to_string();
        let parameters = tool_call
            .arguments
            .as_ref()
            .map(|obj| Value::Object(obj.clone()))
            .unwrap_or(Value::Null);
        Self { name, parameters }
    }
}

#[derive(Debug)]
struct RepetitionState {
    last_call: Option<InternalToolCall>,
    repeat_count: u32,
}

#[derive(Debug)]
struct ErrorState {
    last_tool_name: Option<String>,
    last_error_text: Option<String>,
    consecutive_count: u32,
}

#[derive(Debug)]
pub struct RepetitionInspector {
    max_repetitions: Option<u32>,
    max_same_path_reads: u32,
    state: Mutex<RepetitionState>,
    error_state: Mutex<ErrorState>,
}

/// Check whether `call` is allowed given current state, and update state.
/// Returns false if the call exceeds the repetition limit.
fn check_and_update(
    state: &mut RepetitionState,
    call: &InternalToolCall,
    max_repetitions: Option<u32>,
) -> bool {
    if max_repetitions.is_none() {
        state.last_call = Some(call.clone());
        state.repeat_count = 1;
        return true;
    }

    if let Some(last) = &state.last_call {
        if last.matches(call) {
            state.repeat_count += 1;
            if state.repeat_count > max_repetitions.unwrap() {
                return false;
            }
        } else {
            state.repeat_count = 1;
        }
    } else {
        state.repeat_count = 1;
    }

    state.last_call = Some(call.clone());
    true
}

impl RepetitionInspector {
    pub fn new(max_repetitions: Option<u32>, max_same_path_reads: Option<u32>) -> Self {
        Self {
            max_repetitions,
            max_same_path_reads: max_same_path_reads.unwrap_or(DEFAULT_MAX_SAME_PATH_READS),
            state: Mutex::new(RepetitionState {
                last_call: None,
                repeat_count: 0,
            }),
            error_state: Mutex::new(ErrorState {
                last_tool_name: None,
                last_error_text: None,
                consecutive_count: 0,
            }),
        }
    }

    pub fn record_error(&self, tool_name: &str, error_text: &str) {
        let fingerprint = fingerprint_error(tool_name, error_text);
        let mut state = self.error_state.lock().unwrap();
        if state.last_tool_name.as_deref() == Some(tool_name)
            && state.last_error_text.as_deref() == Some(fingerprint.as_str())
        {
            state.consecutive_count += 1;
        } else {
            state.last_tool_name = Some(tool_name.to_string());
            state.last_error_text = Some(fingerprint);
            state.consecutive_count = 1;
        }
    }

    pub fn record_success(&self) {
        let mut state = self.error_state.lock().unwrap();
        state.last_tool_name = None;
        state.last_error_text = None;
        state.consecutive_count = 0;
    }

    pub fn reset(&self) {
        let mut state = self.state.lock().unwrap();
        state.last_call = None;
        state.repeat_count = 0;
        let mut error_state = self.error_state.lock().unwrap();
        error_state.last_tool_name = None;
        error_state.last_error_text = None;
        error_state.consecutive_count = 0;
    }
}

#[async_trait]
impl ToolInspector for RepetitionInspector {
    fn name(&self) -> &'static str {
        "repetition"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn inspect(
        &self,
        _session_id: &str,
        tool_requests: &[ToolRequest],
        messages: &[Message],
        _goose_mode: GooseMode,
    ) -> Result<Vec<InspectionResult>> {
        let mut results = Vec::new();

        // Check call-repetition limits for each tool request, updating state as we go.
        for tool_request in tool_requests {
            if let Ok(tool_call) = &tool_request.tool_call {
                let internal_call = InternalToolCall::from_tool_call(tool_call);
                let allowed = check_and_update(
                    &mut self.state.lock().unwrap(),
                    &internal_call,
                    self.max_repetitions,
                );
                if !allowed {
                    results.push(InspectionResult {
                        tool_request_id: tool_request.id.clone(),
                        action: InspectionAction::Deny,
                        reason: format!(
                            "Tool '{}' has exceeded maximum repetitions",
                            tool_call.name
                        ),
                        confidence: 1.0,
                        inspector_name: self.name().to_string(),
                        finding_id: Some(FINDING_ID_REPEATED_CALLS.to_string()),
                    });
                }
            }
        }

        // Deny a tool that has returned the same error N consecutive times, regardless
        // of whether call parameters changed — catches retry loops with varying inputs.
        {
            let mut state = self.error_state.lock().unwrap();
            if state.consecutive_count >= MAX_CONSECUTIVE_ERROR_FINGERPRINTS {
                if let Some(ref last_tool) = state.last_tool_name {
                    let error_text = state.last_error_text.as_deref().unwrap_or("");
                    let mut denied = false;
                    for tool_request in tool_requests {
                        if let Ok(tool_call) = &tool_request.tool_call {
                            if tool_call.name.as_ref() == last_tool.as_str() {
                                results.push(InspectionResult {
                                    tool_request_id: tool_request.id.clone(),
                                    action: InspectionAction::Deny,
                                    reason: format!(
                                        "Tool '{}' has returned the same error {} consecutive times: '{}'",
                                        tool_call.name, state.consecutive_count, error_text
                                    ),
                                    confidence: 1.0,
                                    inspector_name: self.name().to_string(),
                                    finding_id: Some(FINDING_ID_REPEATED_ERROR.to_string()),
                                });
                                denied = true;
                            }
                        }
                    }
                    // Reset only after actually denying the failing tool so that an
                    // intervening turn calling other tools doesn't silently clear the streak
                    if denied {
                        state.consecutive_count = 0;
                    }
                }
            }
        }

        // Deny a read tool that has hit the same path too many times without
        // writing to it — catches varying-offset read loops that exact-arg
        // matching misses (e.g. reading config.rs at offset 0, 75, 175, ...).
        for tool_request in tool_requests {
            if let Ok(tool_call) = &tool_request.tool_call {
                if let Some(path) = extract_path_arg(tool_call.arguments.as_ref()) {
                    let count = path_read_count_in_history(messages, tool_call.name.as_ref(), path);
                    if count >= self.max_same_path_reads {
                        results.push(InspectionResult {
                            tool_request_id: tool_request.id.clone(),
                            action: InspectionAction::Deny,
                            reason: format!(
                                "Tool '{}' has read '{}' {} times without writing — try a different approach.",
                                tool_call.name, path, count
                            ),
                            confidence: 1.0,
                            inspector_name: self.name().to_string(),
                            finding_id: Some(FINDING_ID_REPEATED_PATH.to_string()),
                        });
                    }
                }
            }
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::CallToolRequestParams;
    use serde_json::json;

    fn make_tool_request(id: &str, name: &'static str, args: serde_json::Value) -> ToolRequest {
        ToolRequest {
            id: id.to_string(),
            tool_call: Ok(CallToolRequestParams::new(name)
                .with_arguments(args.as_object().cloned().unwrap_or_default())),
            tool_meta: None,
            metadata: None,
        }
    }

    #[tokio::test]
    async fn rep001_fires_after_max_repetitions() {
        let inspector = RepetitionInspector::new(Some(3), None);
        let args = json!({"key": "value"});

        for i in 0..3 {
            let req = make_tool_request(&format!("id-{i}"), "my_tool", args.clone());
            let results = inspector
                .inspect("session", &[req], &[], GooseMode::Auto)
                .await
                .unwrap();
            assert!(results.is_empty(), "call {i} should be allowed");
        }

        let req = make_tool_request("id-3", "my_tool", args.clone());
        let results = inspector
            .inspect("session", &[req], &[], GooseMode::Auto)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].finding_id.as_deref(),
            Some(FINDING_ID_REPEATED_CALLS)
        );
        assert_eq!(results[0].action, InspectionAction::Deny);
    }

    #[tokio::test]
    async fn rep001_resets_on_different_args() {
        let inspector = RepetitionInspector::new(Some(2), None);

        for i in 0..2 {
            let req = make_tool_request(&format!("id-{i}"), "tool", json!({"k": "v1"}));
            let results = inspector
                .inspect("session", &[req], &[], GooseMode::Auto)
                .await
                .unwrap();
            assert!(results.is_empty());
        }

        let req = make_tool_request("id-2", "tool", json!({"k": "v2"}));
        let results = inspector
            .inspect("session", &[req], &[], GooseMode::Auto)
            .await
            .unwrap();
        assert!(results.is_empty(), "different args should reset streak");
    }

    #[tokio::test]
    async fn rep001_disabled_when_max_is_none() {
        let inspector = RepetitionInspector::new(None, None);
        let args = json!({"k": "v"});

        for i in 0..20 {
            let req = make_tool_request(&format!("id-{i}"), "tool", args.clone());
            let results = inspector
                .inspect("session", &[req], &[], GooseMode::Auto)
                .await
                .unwrap();
            assert!(results.is_empty(), "unlimited mode should never deny");
        }
    }

    #[test]
    fn regex_patterns_compile() {
        let _ = &*RUSTC_ERROR_RE;
        let _ = &*TSC_ERROR_RE;
        let _ = &*PYTEST_FAIL_RE;
        let _ = &*CARGO_TEST_FAIL_RE;
    }

    #[test]
    fn fingerprint_collapses_same_rustc_error_across_cargo_progress_noise() {
        let a = "Compiling foo v0.1.0\nerror[E0038]: the trait `Foo` is not dyn compatible\n   --> src/lib.rs:1:1\n";
        let b = "Compiling bar v0.2.0\nerror[E0038]: the trait `Foo` is not dyn compatible\n   --> src/other.rs:42:1\n";
        assert_eq!(
            fingerprint_error("developer__shell", a),
            fingerprint_error("developer__shell", b),
        );
    }

    #[test]
    fn fingerprint_distinguishes_different_traits_under_same_rustc_code() {
        let a = "error[E0038]: the trait `JetStreamContext` is not dyn compatible\n";
        let b = "error[E0038]: the trait `OtherTrait` is not dyn compatible\n";
        assert_ne!(
            fingerprint_error("developer__shell", a),
            fingerprint_error("developer__shell", b),
        );
    }

    #[test]
    fn fingerprint_stops_at_newline() {
        let text = "error[E0038]: the trait `Foo` is not dyn compatible\n   --> src/lib.rs:42:5";
        let fp = fingerprint_error("developer__shell", text);
        assert!(!fp.contains('\n'), "fingerprint must not span lines");
        assert!(
            !fp.contains("-->"),
            "fingerprint must not include line-number arrow"
        );
    }

    #[test]
    fn fingerprint_falls_back_to_prefix_for_non_shell_tools() {
        let text = "error[E0038]: the trait `Foo` is not dyn compatible";
        let fp = fingerprint_error("developer__text_editor", text);
        let expected: String = text.chars().take(100).collect();
        assert_eq!(fp, expected);
    }

    #[tokio::test]
    async fn rep002_fires_after_three_same_rustc_errors() {
        let inspector = RepetitionInspector::new(Some(100), None);
        let tool_name = "developer__shell";
        inspector.record_error(
            tool_name,
            "Compiling foo v0.1.0\nerror[E0038]: the trait `Foo` is not dyn compatible\n",
        );
        inspector.record_error(
            tool_name,
            "Compiling bar v0.2.0\nerror[E0038]: the trait `Foo` is not dyn compatible\n",
        );
        inspector.record_error(
            tool_name,
            "Building [==>] 50/100: baz\nerror[E0038]: the trait `Foo` is not dyn compatible\n",
        );

        let req = make_tool_request(
            "id-1",
            "developer__shell",
            json!({"command": "cargo check"}),
        );
        let results = inspector
            .inspect("session", &[req], &[], GooseMode::Auto)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].finding_id.as_deref(),
            Some(FINDING_ID_REPEATED_ERROR)
        );
    }

    #[tokio::test]
    async fn rep002_does_not_fire_for_distinct_traits_under_same_code() {
        let inspector = RepetitionInspector::new(Some(100), None);
        let tool_name = "developer__shell";
        inspector.record_error(
            tool_name,
            "error[E0038]: the trait `A` is not dyn compatible\n",
        );
        inspector.record_error(
            tool_name,
            "error[E0038]: the trait `B` is not dyn compatible\n",
        );
        inspector.record_error(
            tool_name,
            "error[E0038]: the trait `C` is not dyn compatible\n",
        );

        let req = make_tool_request("id-1", "developer__shell", json!({}));
        let results = inspector
            .inspect("session", &[req], &[], GooseMode::Auto)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    fn read_request_msg(id: &str, path: &str) -> Message {
        let args = json!({"path": path});
        let tool_call = Ok(CallToolRequestParams::new("read")
            .with_arguments(args.as_object().cloned().unwrap_or_default()));
        Message::assistant().with_tool_request(id, tool_call)
    }

    fn write_request_msg(id: &str, path: &str) -> Message {
        let args = json!({"path": path});
        let tool_call = Ok(CallToolRequestParams::new("write")
            .with_arguments(args.as_object().cloned().unwrap_or_default()));
        Message::assistant().with_tool_request(id, tool_call)
    }

    #[tokio::test]
    async fn rep003_fires_at_default_threshold_of_five() {
        let inspector = RepetitionInspector::new(Some(100), None);
        let path = "/tmp/foo.rs";
        let mut history: Vec<Message> = (0..4)
            .map(|i| read_request_msg(&format!("r-{i}"), path))
            .collect();
        // The current call (5th read) is in tool_requests, not in history.
        let req = make_tool_request("r-4", "read", json!({"path": path}));
        history.push(read_request_msg("r-4", path));

        let results = inspector
            .inspect("session", &[req], &history, GooseMode::Auto)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].finding_id.as_deref(),
            Some(FINDING_ID_REPEATED_PATH)
        );
    }

    #[tokio::test]
    async fn rep003_threshold_is_tunable() {
        let inspector = RepetitionInspector::new(Some(100), Some(3));
        let path = "/tmp/foo.rs";
        let history: Vec<Message> = (0..3)
            .map(|i| read_request_msg(&format!("r-{i}"), path))
            .collect();
        let req = make_tool_request("r-cur", "read", json!({"path": path}));

        let results = inspector
            .inspect("session", &[req], &history, GooseMode::Auto)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].finding_id.as_deref(),
            Some(FINDING_ID_REPEATED_PATH)
        );
    }

    #[tokio::test]
    async fn rep003_resets_on_intervening_write() {
        let inspector = RepetitionInspector::new(Some(100), None);
        let path = "/tmp/foo.rs";
        let mut history: Vec<Message> = (0..3)
            .map(|i| read_request_msg(&format!("r-{i}"), path))
            .collect();
        history.push(write_request_msg("w-0", path));
        history.push(read_request_msg("r-3", path));
        let req = make_tool_request("r-cur", "read", json!({"path": path}));

        let results = inspector
            .inspect("session", &[req], &history, GooseMode::Auto)
            .await
            .unwrap();
        assert!(
            results.is_empty(),
            "write between reads should reset REP-003 counter"
        );
    }
}
