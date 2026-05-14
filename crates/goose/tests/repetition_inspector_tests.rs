use goose::tool_inspection::{InspectionAction, ToolInspector};
use goose::tool_monitor::RepetitionInspector;
use rmcp::model::CallToolRequestParams;

fn make_tool_request_with_args(
    id: &str,
    tool_name: &'static str,
    args: serde_json::Value,
) -> goose::conversation::message::ToolRequest {
    goose::conversation::message::ToolRequest {
        id: id.to_string(),
        tool_call: Ok(CallToolRequestParams::new(tool_name)
            .with_arguments(args.as_object().cloned().unwrap_or_default())),
        metadata: None,
        tool_meta: None,
    }
}

fn make_tool_request(tool_name: &'static str) -> goose::conversation::message::ToolRequest {
    make_tool_request_with_args(
        &format!("req_{}", tool_name),
        tool_name,
        serde_json::json!({}),
    )
}

async fn run_inspect(
    inspector: &RepetitionInspector,
    id: &str,
    tool_name: &'static str,
    args: serde_json::Value,
) -> Vec<goose::tool_inspection::InspectionResult> {
    let req = make_tool_request_with_args(id, tool_name, args);
    ToolInspector::inspect(
        inspector,
        "session",
        &[req],
        &[],
        goose::config::GooseMode::Auto,
    )
    .await
    .unwrap()
}

/// REP-001: consecutive identical tool calls are allowed up to max_repetitions times;
/// the (max_repetitions + 1)th identical call is denied; changing parameters resets the streak.
#[tokio::test]
async fn test_repetition_inspector_denies_after_exceeding_and_resets_on_param_change() {
    let inspector = RepetitionInspector::new(Some(2), None);
    let v1 = serde_json::json!({"id": 123});
    let v2 = serde_json::json!({"id": 456});

    // First identical call → allowed
    assert!(run_inspect(&inspector, "r1", "fetch_user", v1.clone())
        .await
        .is_empty());
    // Second identical call → still allowed (at limit)
    assert!(run_inspect(&inspector, "r2", "fetch_user", v1.clone())
        .await
        .is_empty());
    // Third identical call → denied (exceeds limit)
    let denied = run_inspect(&inspector, "r3", "fetch_user", v1).await;
    assert_eq!(denied.len(), 1);
    assert_eq!(denied[0].finding_id.as_deref(), Some("REP-001"));

    // Change parameters; consecutive counter resets, call is allowed
    assert!(run_inspect(&inspector, "r4", "fetch_user", v2.clone())
        .await
        .is_empty());
    // Another identical call with new params → allowed (second in a row)
    assert!(run_inspect(&inspector, "r5", "fetch_user", v2.clone())
        .await
        .is_empty());
    // One more identical call with new params → denied again
    let denied2 = run_inspect(&inspector, "r6", "fetch_user", v2).await;
    assert_eq!(denied2.len(), 1);
    assert_eq!(denied2[0].finding_id.as_deref(), Some("REP-001"));
}

#[tokio::test]
async fn test_error_pattern_fires_after_threshold() {
    let inspector = RepetitionInspector::new(None, None);
    for _ in 0..3 {
        inspector.record_error("req-err", "my_tool", "404 Not Found");
    }
    let requests = vec![make_tool_request("my_tool")];
    let results: Vec<_> = ToolInspector::inspect(
        &inspector,
        "session",
        &requests,
        &[],
        goose::config::GooseMode::Auto,
    )
    .await
    .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].action, InspectionAction::Deny);
    assert_eq!(results[0].finding_id.as_deref(), Some("REP-002"));
}

#[tokio::test]
async fn test_error_pattern_does_not_fire_before_threshold() {
    let inspector = RepetitionInspector::new(None, None);
    for _ in 0..2 {
        inspector.record_error("req-err", "my_tool", "404 Not Found");
    }
    let requests = vec![make_tool_request("my_tool")];
    let results: Vec<_> = ToolInspector::inspect(
        &inspector,
        "session",
        &requests,
        &[],
        goose::config::GooseMode::Auto,
    )
    .await
    .unwrap();
    assert!(results
        .iter()
        .all(|r| r.finding_id.as_deref() != Some("REP-002")));
}

#[tokio::test]
async fn test_error_pattern_resets_on_success() {
    let inspector = RepetitionInspector::new(None, None);
    inspector.record_error("req-err", "my_tool", "404 Not Found");
    inspector.record_error("req-err", "my_tool", "404 Not Found");
    inspector.record_success();
    inspector.record_error("req-err", "my_tool", "404 Not Found");
    let requests = vec![make_tool_request("my_tool")];
    let results: Vec<_> = ToolInspector::inspect(
        &inspector,
        "session",
        &requests,
        &[],
        goose::config::GooseMode::Auto,
    )
    .await
    .unwrap();
    assert!(results
        .iter()
        .all(|r| r.finding_id.as_deref() != Some("REP-002")));
}

#[tokio::test]
async fn test_error_pattern_streak_not_cleared_by_unrelated_tool() {
    let inspector = RepetitionInspector::new(None, None);
    for _ in 0..3 {
        inspector.record_error("req-err", "tool_a", "timeout");
    }
    // Calling an unrelated tool must not reset tool_a's streak
    let unrelated = vec![make_tool_request("tool_b")];
    ToolInspector::inspect(
        &inspector,
        "session",
        &unrelated,
        &[],
        goose::config::GooseMode::Auto,
    )
    .await
    .unwrap();
    // tool_a should still be denied on the next call
    let requests = vec![make_tool_request("tool_a")];
    let results: Vec<_> = ToolInspector::inspect(
        &inspector,
        "session",
        &requests,
        &[],
        goose::config::GooseMode::Auto,
    )
    .await
    .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].action, InspectionAction::Deny);
    assert_eq!(results[0].finding_id.as_deref(), Some("REP-002"));
}

#[tokio::test]
async fn test_error_pattern_does_not_cross_tool_names() {
    let inspector = RepetitionInspector::new(None, None);
    for _ in 0..3 {
        inspector.record_error("req-err", "tool_a", "same error");
    }
    let requests = vec![make_tool_request("tool_b")];
    let results: Vec<_> = ToolInspector::inspect(
        &inspector,
        "session",
        &requests,
        &[],
        goose::config::GooseMode::Auto,
    )
    .await
    .unwrap();
    assert!(results
        .iter()
        .all(|r| r.finding_id.as_deref() != Some("REP-002")));
}
