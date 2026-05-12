import { beforeEach, describe, expect, it } from "vitest";
import type { SessionNotification } from "@agentclientprotocol/sdk";
import { useChatStore } from "@/features/chat/stores/chatStore";
import { clearReplayBuffer } from "@/features/chat/hooks/replayBuffer";
import {
  clearMessageTracking,
  handleSessionNotification,
} from "./acpNotificationHandler";

describe("acpNotificationHandler", () => {
  beforeEach(() => {
    clearMessageTracking();
    clearReplayBuffer("acp-session-1");
    clearReplayBuffer("acp-session-2");
    useChatStore.setState({
      messagesBySession: {},
      sessionStateById: {},
      queuedMessageBySession: {},
      draftsBySession: {},
      activeSessionId: null,
      isConnected: false,
      loadingSessionIds: new Set<string>(),
      scrollTargetMessageBySession: {},
    });
  });

  it("maps usage updates to context fields and reads accumulated from _meta", async () => {
    const notification = {
      sessionId: "acp-session-1",
      update: {
        sessionUpdate: "usage_update",
        used: 512,
        size: 8192,
        _meta: {
          "goose.accumulatedTotal": 12_345,
          "goose.accumulatedInput": 11_000,
          "goose.accumulatedOutput": 1_345,
        },
      },
    } as SessionNotification;

    await handleSessionNotification(notification);

    const runtime = useChatStore.getState().getSessionRuntime("acp-session-1");
    expect(runtime.tokenState.totalTokens).toBe(512);
    expect(runtime.tokenState.contextLimit).toBe(8192);
    expect(runtime.tokenState.accumulatedTotal).toBe(12_345);
    expect(runtime.tokenState.accumulatedInput).toBe(11_000);
    expect(runtime.tokenState.accumulatedOutput).toBe(1_345);
    expect(runtime.hasUsageSnapshot).toBe(true);
  });

  it("defaults accumulated fields to zero when _meta is absent", async () => {
    const notification = {
      sessionId: "acp-session-1",
      update: {
        sessionUpdate: "usage_update",
        used: 100,
        size: 8192,
      },
    } as SessionNotification;

    await handleSessionNotification(notification);

    const runtime = useChatStore.getState().getSessionRuntime("acp-session-1");
    expect(runtime.tokenState.totalTokens).toBe(100);
    expect(runtime.tokenState.accumulatedTotal).toBe(0);
    expect(runtime.tokenState.accumulatedInput).toBe(0);
    expect(runtime.tokenState.accumulatedOutput).toBe(0);
  });
});
