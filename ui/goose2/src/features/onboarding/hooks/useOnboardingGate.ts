import { useCallback, useMemo, useState } from "react";
import type { ProviderInventoryEntryDto } from "@aaif/goose-sdk";
import { useAgentStore } from "@/features/agents/stores/agentStore";
import {
  getModelProvidersFromEntries,
  resolveAgentProviderCatalogIdStrictFromEntries,
} from "@/features/providers/providerCatalog";
import { useProviderInventory } from "@/features/providers/hooks/useProviderInventory";
import { useProviderCatalogStore } from "@/features/providers/stores/providerCatalogStore";
import { useDistroStore } from "@/features/settings/stores/distroStore";
import { filterModelProvidersForDistro } from "@/features/providers/distroProviderConstraints";
import { getStoredModelPreference } from "@/features/chat/lib/modelPreferences";
import {
  ONBOARDING_RESET_REQUESTED_KEY,
  ONBOARDING_STORAGE_KEY,
  type OnboardingReadiness,
} from "../types";

function readResetRequested(): boolean {
  return localStorage.getItem(ONBOARDING_RESET_REQUESTED_KEY) === "1";
}

function writeResetRequested(value: boolean) {
  if (value) {
    localStorage.setItem(ONBOARDING_RESET_REQUESTED_KEY, "1");
  } else {
    localStorage.removeItem(ONBOARDING_RESET_REQUESTED_KEY);
  }
}

export function resetOnboardingCompletion() {
  writeResetRequested(true);
  // Opportunistic cleanup of legacy keys from the prior gate design.
  localStorage.removeItem(ONBOARDING_STORAGE_KEY);
  localStorage.removeItem("goose:onboarding:grandfathered:v1");
}

function firstUsableModel(entry: ProviderInventoryEntryDto) {
  return (
    entry.models.find((model) => model.recommended) ??
    entry.models.find((model) => model.id === entry.defaultModel) ??
    entry.models[0]
  );
}

export function useOnboardingGate(startupReady: boolean) {
  const selectedProvider = useAgentStore((state) => state.selectedProvider);
  const { entries, configuredModelProviderEntries, getModelsForAgent } =
    useProviderInventory();
  const catalogEntries = useProviderCatalogStore((state) => state.entries);
  const distro = useDistroStore((state) => state.manifest);
  const [resetRequested, setResetRequested] =
    useState<boolean>(readResetRequested);

  // Subscribe to catalogEntries so the memo recomputes when the catalog
  // populates from the backend. The previous version called getModelProviders()
  // (which reads via .getState()) and only listed `distro` as a dependency,
  // which left modelProviderIds permanently empty and broke the gate's
  // configuredEntry fallback for users with a working provider.
  const modelProviderIds = useMemo(
    () =>
      new Set(
        filterModelProvidersForDistro(
          getModelProvidersFromEntries(catalogEntries),
          distro,
        ).map((provider) => provider.id),
      ),
    [catalogEntries, distro],
  );

  const readiness = useMemo<OnboardingReadiness>(() => {
    const selectedAgentId =
      resolveAgentProviderCatalogIdStrictFromEntries(
        catalogEntries,
        selectedProvider,
      ) ?? "goose";

    if (selectedAgentId !== "goose") {
      const models = getModelsForAgent(selectedAgentId);
      const entry = entries.get(selectedAgentId);
      const isReady = !!entry?.configured && models.length > 0;
      if (isReady) {
        return {
          isUsable: true,
          providerId: selectedAgentId,
          modelId: models[0]?.id,
          modelName: models[0]?.name,
          reason: "ready",
        };
      }
    }

    const storedGooseModel = getStoredModelPreference("goose");
    if (storedGooseModel) {
      const entry = entries.get(storedGooseModel.providerId ?? "");
      const modelStillExists = entry?.models.some(
        (model) => model.id === storedGooseModel.modelId,
      );
      if (entry?.configured && modelStillExists) {
        return {
          isUsable: true,
          providerId: storedGooseModel.providerId ?? "goose",
          modelId: storedGooseModel.modelId,
          modelName: storedGooseModel.modelName,
          reason: "ready",
        };
      }
    }

    const configuredEntry =
      configuredModelProviderEntries.find(
        (entry) =>
          (entry.providerType === "Custom" ||
            modelProviderIds.has(entry.providerId)) &&
          firstUsableModel(entry),
      ) ?? null;
    const model = configuredEntry ? firstUsableModel(configuredEntry) : null;

    if (configuredEntry && model) {
      return {
        isUsable: true,
        providerId: configuredEntry.providerId,
        modelId: model.id,
        modelName: model.name,
        reason: "ready",
      };
    }

    return {
      isUsable: false,
      providerId: null,
      reason: "missing_provider",
    };
  }, [
    catalogEntries,
    configuredModelProviderEntries,
    entries,
    getModelsForAgent,
    modelProviderIds,
    selectedProvider,
  ]);

  const completeOnboarding = useCallback(() => {
    writeResetRequested(false);
    setResetRequested(false);
  }, []);

  const resetOnboarding = useCallback(() => {
    resetOnboardingCompletion();
    setResetRequested(true);
  }, []);

  return {
    readiness,
    shouldShowOnboarding:
      startupReady && (resetRequested || !readiness.isUsable),
    completeOnboarding,
    resetOnboarding,
  };
}
