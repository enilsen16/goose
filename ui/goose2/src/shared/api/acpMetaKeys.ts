// Goose-namespaced ACP `_meta` keys. Mirrored on the backend in
// `crates/goose/src/acp/server.rs` — keep in sync.

export const META_ACCUMULATED_TOTAL = "goose.accumulatedTotal";
export const META_ACCUMULATED_INPUT = "goose.accumulatedInput";
export const META_ACCUMULATED_OUTPUT = "goose.accumulatedOutput";

/// Carried on `SessionInfoUpdate._meta` to forward transient agent-side
/// notifications (e.g. "goose is compacting the conversation...") to the UI
/// in real time. Shape: `{ type: string, msg: string }` where `type` is one of
/// `"thinkingMessage" | "inlineMessage" | "creditsExhausted"`.
export const META_SYSTEM_NOTIFICATION = "goose.systemNotification";
