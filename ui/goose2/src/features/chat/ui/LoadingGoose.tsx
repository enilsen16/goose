import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { motion, useReducedMotion } from "motion/react";
import type { ActiveToolEntry } from "@/shared/types/chat";
import { Shimmer } from "@/shared/ui/ai-elements/shimmer";

export type LoadingChatState =
  | "idle"
  | "thinking"
  | "streaming"
  | "waiting"
  | "compacting";

interface LoadingGooseProps {
  chatState?: LoadingChatState;
  activeTool?: Pick<ActiveToolEntry, "name" | "startedAt">;
}

const LOADING_FADE_S = 0.45;
const LOADING_SHIMMER_S = 3;
const LOADING_SHIMMER_SPREAD = 3;
const LOADING_SHIMMER_DELAY_S = 0.35;
const LOADING_SHIMMER_REPEAT_DELAY_S = 0.9;
const ELAPSED_THRESHOLD_S = 4;

const MESSAGE_KEY_BY_STATE: Record<
  Exclude<LoadingChatState, "idle">,
  "thinking" | "responding" | "compacting"
> = {
  thinking: "thinking",
  streaming: "responding",
  waiting: "responding",
  compacting: "compacting",
};

function formatToolName(name: string): string {
  return name.replace(/_+/g, " ").trim();
}

function useElapsedSeconds(startedAt?: number): number {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (startedAt == null) return;
    setNow(Date.now());
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [startedAt]);

  if (startedAt == null) return 0;
  return Math.max(0, Math.floor((now - startedAt) / 1000));
}

export function LoadingGoose({
  chatState = "idle",
  activeTool,
}: LoadingGooseProps) {
  const { t } = useTranslation("chat");
  const shouldReduceMotion = useReducedMotion();
  const elapsed = useElapsedSeconds(activeTool?.startedAt);

  if (chatState === "idle") {
    return null;
  }

  const message = activeTool
    ? t("loading.callingTool", { name: formatToolName(activeTool.name) })
    : t(`loading.${MESSAGE_KEY_BY_STATE[chatState]}`);

  const showElapsed = activeTool != null && elapsed >= ELAPSED_THRESHOLD_S;

  return (
    <motion.div
      className="px-4"
      role="status"
      aria-live="polite"
      aria-label={message}
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      transition={{ duration: shouldReduceMotion ? 0 : LOADING_FADE_S }}
    >
      <div className="max-w-3xl mx-auto w-full">
        <div className="py-2 text-xs text-muted-foreground">
          {shouldReduceMotion ? (
            <span>{message}</span>
          ) : (
            <Shimmer
              as="span"
              className="text-xs"
              tone="soft"
              delay={LOADING_SHIMMER_DELAY_S}
              duration={LOADING_SHIMMER_S}
              spread={LOADING_SHIMMER_SPREAD}
              repeatDelay={LOADING_SHIMMER_REPEAT_DELAY_S}
            >
              {message}
            </Shimmer>
          )}
          {showElapsed ? (
            // i18n-check-ignore: numeric seconds abbreviation is universal
            <span aria-hidden="true">{` ${elapsed}s`}</span>
          ) : null}
        </div>
      </div>
    </motion.div>
  );
}
