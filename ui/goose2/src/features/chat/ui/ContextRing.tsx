import { useTranslation } from "react-i18next";
import { useLocaleFormatting } from "@/shared/i18n";

// ---------------------------------------------------------------------------
// ContextRing — SVG circular indicator for context token usage
//
// Stroke color tracks fill ratio with the same thresholds goose1 uses
// (ui/desktop/.../ContextWindowIndicator.tsx): ≤75% foreground, 75–90% warning,
// >90% danger. The change gives the user a visual nudge before the auto-compact
// threshold (default 80%) kicks in.
// ---------------------------------------------------------------------------

const CONTEXT_WARN_THRESHOLD = 0.75;
const CONTEXT_DANGER_THRESHOLD = 0.9;

function strokeColorFor(progress: number): string {
  if (progress >= CONTEXT_DANGER_THRESHOLD) return "var(--text-danger)";
  if (progress >= CONTEXT_WARN_THRESHOLD) return "var(--text-warning)";
  return "var(--color-foreground)";
}

export function ContextRing({
  tokens,
  limit,
  size = 20,
}: {
  tokens: number;
  limit: number;
  size?: number;
}) {
  const { t } = useTranslation("chat");
  const { formatNumber } = useLocaleFormatting();
  const radius = (size - 3) / 2;
  const circumference = 2 * Math.PI * radius;
  const progress = limit > 0 ? Math.min(tokens / limit, 1) : 0;
  const offset = circumference - progress * circumference;
  const percent = formatNumber(Math.round(progress * 100));
  const stroke = strokeColorFor(progress);

  return (
    <svg
      width={size}
      height={size}
      viewBox={`0 0 ${size} ${size}`}
      className="shrink-0"
      aria-label={t("context.ringAria", { percent })}
    >
      <circle
        cx={size / 2}
        cy={size / 2}
        r={radius}
        fill="none"
        stroke="var(--color-border)"
        strokeWidth={2.5}
      />
      <circle
        cx={size / 2}
        cy={size / 2}
        r={radius}
        fill="none"
        stroke={stroke}
        strokeWidth={2.5}
        strokeLinecap={progress > 0 ? "round" : "butt"}
        strokeDasharray={circumference}
        strokeDashoffset={offset}
        transform={`rotate(-90 ${size / 2} ${size / 2})`}
        className="transition-all duration-300 ease-out"
      />
    </svg>
  );
}
