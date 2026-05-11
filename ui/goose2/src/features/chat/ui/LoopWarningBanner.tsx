import { IconAlertTriangle } from "@tabler/icons-react";
import { useTranslation } from "react-i18next";
import { useChatStore } from "../stores/chatStore";

interface LoopWarningBannerProps {
  sessionId: string;
  onStop: () => void;
}

export function LoopWarningBanner({
  sessionId,
  onStop,
}: LoopWarningBannerProps) {
  const { t } = useTranslation("chat");
  const loopWarning = useChatStore(
    (s) => s.sessionStateById[sessionId]?.loopWarning ?? null,
  );
  const clearLoopWarning = useChatStore((s) => s.clearLoopWarning);

  if (!loopWarning) return null;

  return (
    <div className="mx-2 mb-2 flex items-center gap-2 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
      <IconAlertTriangle className="h-4 w-4 shrink-0" />
      <span className="flex-1">
        {t("loopWarning.message", {
          toolName: loopWarning.toolName,
          count: loopWarning.count,
        })}
      </span>
      <button
        type="button"
        className="shrink-0 text-xs text-foreground-secondary hover:text-foreground"
        onClick={() => clearLoopWarning(sessionId)}
      >
        {t("loopWarning.dismiss")}
      </button>
      <button
        type="button"
        className="shrink-0 rounded bg-warning/20 px-2 py-0.5 text-xs font-medium hover:bg-warning/30"
        onClick={onStop}
      >
        {t("loopWarning.stop")}
      </button>
    </div>
  );
}
