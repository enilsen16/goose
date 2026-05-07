import { act, renderHook } from "@testing-library/react";
import type { ProviderInventoryEntryDto } from "@aaif/goose-sdk";
import { beforeEach, describe, expect, it } from "vitest";
import { useAgentStore } from "@/features/agents/stores/agentStore";
import { setStoredModelPreference } from "@/features/chat/lib/modelPreferences";
import { useProviderCatalogStore } from "@/features/providers/stores/providerCatalogStore";
import { useProviderInventoryStore } from "@/features/providers/stores/providerInventoryStore";
import { ONBOARDING_RESET_REQUESTED_KEY } from "../types";
import { useOnboardingGate } from "./useOnboardingGate";

function providerEntry(
  overrides: Partial<ProviderInventoryEntryDto>,
): ProviderInventoryEntryDto {
  const providerId = overrides.providerId ?? "anthropic";

  return {
    providerId,
    providerName: overrides.providerName ?? providerId,
    description: "",
    defaultModel: "claude-sonnet-4-5",
    configured: true,
    providerType: "Preferred",
    category: "model",
    configKeys: [],
    setupSteps: [],
    supportsRefresh: true,
    refreshing: false,
    models: [
      {
        id: "claude-sonnet-4-5",
        name: "Claude Sonnet 4.5",
        family: "claude",
        contextLimit: 200000,
        recommended: true,
      },
    ],
    stale: false,
    ...overrides,
  };
}

describe("useOnboardingGate", () => {
  beforeEach(() => {
    window.localStorage.clear();
    useAgentStore.setState({
      selectedProvider: "goose",
      providers: [],
    });
    useProviderInventoryStore.setState({
      entries: new Map(),
      loading: false,
    });
    useProviderCatalogStore.getState().setEntries([
      {
        id: "anthropic",
        displayName: "Anthropic",
        category: "model",
        description: "",
        setupMethod: "single_api_key",
        group: "default",
      },
      {
        id: "claude-acp",
        displayName: "Claude Code",
        category: "agent",
        description: "",
        setupMethod: "cli_auth",
        group: "additional",
      },
    ]);
  });

  it("skips onboarding when the user has a usable provider", () => {
    useProviderInventoryStore.getState().setEntries([providerEntry({})]);

    const { result } = renderHook(() => useOnboardingGate(true));

    expect(result.current.shouldShowOnboarding).toBe(false);
    expect(result.current.readiness.isUsable).toBe(true);
    expect(result.current.readiness.reason).toBe("ready");
  });

  it("reopens onboarding after an explicit reset, even with a usable provider", () => {
    useProviderInventoryStore.getState().setEntries([providerEntry({})]);

    const { result, rerender } = renderHook(() => useOnboardingGate(true));
    expect(result.current.shouldShowOnboarding).toBe(false);

    act(() => result.current.resetOnboarding());
    rerender();

    expect(result.current.shouldShowOnboarding).toBe(true);
    expect(window.localStorage.getItem(ONBOARDING_RESET_REQUESTED_KEY)).toBe(
      "1",
    );
  });

  it("shows onboarding for new users with no usable provider", () => {
    useProviderInventoryStore.getState().setEntries([
      providerEntry({
        configured: false,
        models: [],
      }),
    ]);

    const { result } = renderHook(() => useOnboardingGate(true));

    expect(result.current.shouldShowOnboarding).toBe(true);
    expect(result.current.readiness.isUsable).toBe(false);
    expect(result.current.readiness.reason).toBe("missing_provider");
  });

  it("skips onboarding when the selected Goose model is usable", () => {
    setStoredModelPreference("goose", {
      providerId: "anthropic",
      modelId: "claude-sonnet-4-5",
      modelName: "Claude Sonnet 4.5",
    });
    useProviderInventoryStore.getState().setEntries([providerEntry({})]);

    const { result } = renderHook(() => useOnboardingGate(true));

    expect(result.current.shouldShowOnboarding).toBe(false);
    expect(result.current.readiness.isUsable).toBe(true);
    expect(result.current.readiness.providerId).toBe("anthropic");
  });

  it("reopens onboarding when the Goose provider is no longer usable", () => {
    setStoredModelPreference("goose", {
      providerId: "anthropic",
      modelId: "claude-sonnet-4-5",
      modelName: "Claude Sonnet 4.5",
    });
    useProviderInventoryStore.getState().setEntries([
      providerEntry({
        configured: false,
        models: [],
      }),
    ]);

    const { result } = renderHook(() => useOnboardingGate(true));

    expect(result.current.shouldShowOnboarding).toBe(true);
    expect(result.current.readiness.isUsable).toBe(false);
    expect(result.current.readiness.reason).toBe("missing_provider");
  });

  it("treats an ACP agent provider with models as usable", () => {
    useAgentStore.setState({ selectedProvider: "claude-acp" });
    useProviderInventoryStore.getState().setEntries([
      providerEntry({
        providerId: "claude-acp",
        providerName: "Claude Code",
        providerType: "Acp",
        category: "agent",
        defaultModel: "claude-acp-session",
        models: [
          {
            id: "claude-acp-session",
            name: "Claude Code",
            family: "acp",
            contextLimit: null,
            recommended: true,
          },
        ],
      }),
    ]);

    const { result } = renderHook(() => useOnboardingGate(true));

    expect(result.current.shouldShowOnboarding).toBe(false);
    expect(result.current.readiness.providerId).toBe("claude-acp");
    expect(result.current.readiness.reason).toBe("ready");
  });

  it("falls back to a usable Goose model when the selected ACP agent is unusable", () => {
    setStoredModelPreference("goose", {
      providerId: "anthropic",
      modelId: "claude-sonnet-4-5",
      modelName: "Claude Sonnet 4.5",
    });
    useAgentStore.setState({ selectedProvider: "claude-acp" });
    useProviderInventoryStore.getState().setEntries([
      providerEntry({}),
      providerEntry({
        providerId: "claude-acp",
        providerName: "Claude Code",
        providerType: "Acp",
        category: "agent",
        defaultModel: "claude-acp-session",
        configured: false,
        models: [],
      }),
    ]);

    const { result } = renderHook(() => useOnboardingGate(true));

    expect(result.current.shouldShowOnboarding).toBe(false);
    expect(result.current.readiness.isUsable).toBe(true);
    expect(result.current.readiness.providerId).toBe("anthropic");
    expect(result.current.readiness.reason).toBe("ready");
  });

  it("clears the reset flag when onboarding completes", () => {
    window.localStorage.setItem(ONBOARDING_RESET_REQUESTED_KEY, "1");
    useProviderInventoryStore.getState().setEntries([providerEntry({})]);

    const { result } = renderHook(() => useOnboardingGate(true));
    expect(result.current.shouldShowOnboarding).toBe(true);

    act(() => {
      result.current.completeOnboarding();
    });

    expect(
      window.localStorage.getItem(ONBOARDING_RESET_REQUESTED_KEY),
    ).toBeNull();
    expect(result.current.shouldShowOnboarding).toBe(false);
  });
});
