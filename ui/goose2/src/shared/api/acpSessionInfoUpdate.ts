import type { SessionUpdate } from "@agentclientprotocol/sdk";
import { useChatSessionStore } from "@/features/chat/stores/chatSessionStore";
import { useChatStore } from "@/features/chat/stores/chatStore";
import { META_SYSTEM_NOTIFICATION } from "./acpMetaKeys";

type SessionInfoUpdate = SessionUpdate & {
  sessionUpdate: "session_info_update";
  title?: unknown;
  updatedAt?: unknown;
  _meta?: unknown;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function applySystemNotification(
  sessionId: string,
  payload: Record<string, unknown>,
): void {
  // `event` is a stable backend-classified kind; `type`/`msg` are forwarded
  // for forward-compat / debugging but not switched on here.
  const event = typeof payload.event === "string" ? payload.event : "";
  if (event === "compacting") {
    useChatStore.getState().setChatState(sessionId, "compacting");
  }
}

export function handleSessionInfoUpdate(
  sessionId: string,
  update: SessionUpdate,
): void {
  const info = update as SessionInfoUpdate;
  const meta = isRecord(info._meta) ? info._meta : {};

  // Transient agent signals (e.g. "compacting") are independent of the
  // chatSessionStore — apply them before the early-return guard below so
  // they fire even for sessions that haven't been hydrated into that store.
  const sysNotification = meta[META_SYSTEM_NOTIFICATION];
  if (isRecord(sysNotification)) {
    applySystemNotification(sessionId, sysNotification);
  }

  const sessionStore = useChatSessionStore.getState();
  const session = sessionStore.getSession(sessionId);
  if (!session) {
    return;
  }

  const patch: Parameters<typeof sessionStore.patchSession>[1] = {};

  if (typeof info.title === "string" && info.title && !session.userSetName) {
    patch.title = info.title;
  }
  if (typeof info.updatedAt === "string" && info.updatedAt) {
    patch.updatedAt = info.updatedAt;
  }
  if (typeof meta.messageCount === "number") {
    patch.messageCount = meta.messageCount;
  }
  if (typeof meta.userSetName === "boolean") {
    patch.userSetName = meta.userSetName;
  }

  if (Object.keys(patch).length > 0) {
    sessionStore.patchSession(sessionId, patch);
  }
}
