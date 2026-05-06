import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { LoadingGoose } from "../LoadingGoose";
import chat from "@/shared/i18n/locales/en/chat.json";

const { thinking, responding, compacting } = chat.loading;

describe("LoadingGoose", () => {
  it("renders thinking copy for the thinking state", () => {
    render(<LoadingGoose chatState="thinking" />);

    expect(screen.getByRole("status", { name: thinking })).toBeInTheDocument();
  });

  it("renders responding copy for active response states", () => {
    const { rerender } = render(<LoadingGoose chatState="streaming" />);

    expect(
      screen.getByRole("status", { name: responding }),
    ).toBeInTheDocument();

    rerender(<LoadingGoose chatState="waiting" />);
    expect(
      screen.getByRole("status", { name: responding }),
    ).toBeInTheDocument();
  });

  it("renders compacting copy for the compacting state", () => {
    render(<LoadingGoose chatState="compacting" />);

    expect(
      screen.getByRole("status", { name: compacting }),
    ).toBeInTheDocument();
  });

  it("renders nothing while idle", () => {
    const { container } = render(<LoadingGoose chatState="idle" />);

    expect(container).toBeEmptyDOMElement();
  });

  describe("active tool override", () => {
    beforeEach(() => {
      vi.useFakeTimers({ now: 1_000_000 });
    });

    afterEach(() => {
      vi.useRealTimers();
    });

    it("shows the active tool name with snake_case formatting", () => {
      render(
        <LoadingGoose
          chatState="streaming"
          activeTool={{ name: "execute_typescript", startedAt: 1_000_000 }}
        />,
      );

      expect(
        screen.getByRole("status", { name: "Calling execute typescript..." }),
      ).toBeInTheDocument();
      expect(screen.queryByText(/\d+s/)).not.toBeInTheDocument();
    });

    it("collapses consecutive underscores in tool names", () => {
      render(
        <LoadingGoose
          chatState="streaming"
          activeTool={{ name: "foo__bar", startedAt: 1_000_000 }}
        />,
      );

      expect(
        screen.getByRole("status", { name: "Calling foo bar..." }),
      ).toBeInTheDocument();
    });

    it("does not show elapsed counter before threshold", () => {
      render(
        <LoadingGoose
          chatState="streaming"
          activeTool={{ name: "shell", startedAt: 1_000_000 - 2_000 }}
        />,
      );

      expect(screen.queryByText(/\d+s/)).not.toBeInTheDocument();
    });

    it("appears at threshold and ticks", () => {
      render(
        <LoadingGoose
          chatState="streaming"
          activeTool={{ name: "shell", startedAt: 1_000_000 - 4_000 }}
        />,
      );

      expect(screen.getByText("4s")).toBeInTheDocument();

      act(() => {
        vi.advanceTimersByTime(2_000);
      });

      expect(screen.getByText("6s")).toBeInTheDocument();
    });

    it("hides the elapsed counter from screen readers", () => {
      render(
        <LoadingGoose
          chatState="streaming"
          activeTool={{ name: "shell", startedAt: 1_000_000 - 5_000 }}
        />,
      );

      const elapsed = screen.getByText("5s");
      expect(elapsed).toHaveAttribute("aria-hidden", "true");
    });

    it("uses aria-live=polite on the status container", () => {
      render(
        <LoadingGoose
          chatState="streaming"
          activeTool={{ name: "shell", startedAt: 1_000_000 }}
        />,
      );

      const status = screen.getByRole("status");
      expect(status).toHaveAttribute("aria-live", "polite");
    });
  });
});
