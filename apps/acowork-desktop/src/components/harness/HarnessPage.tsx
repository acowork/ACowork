import { useState, useEffect, useCallback, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { VaultKeyEntry, ModelInfo, ModelCapabilitiesInfo, ProviderListEntry, McpServerConfigDef, McpTransportDef, McpPresetDef } from "../../lib/types";
import { cn } from "../../lib/utils";
import { inputBase } from "../../lib/ui-styles";
import { StyledInput } from "../common/StyledInput";
import { Dropdown } from "../common/Dropdown";
import { ProviderLogo } from "../common/ProviderLogo";
import { isLocalProvider } from "../../lib/providers";
import { fetchProviderModels } from "../../lib/gateway-api";
import { getGatewayUrl } from "../../lib/config";
import { Monitor, MousePointer, Package, Search, Globe, BookOpen, FileText, PenTool, Star, Plus, CheckCircle2, Download, XCircle, Loader2, Minus } from "lucide-react";
import { useMcpStore, type McpInstallRunResponse } from "../../stores/mcpStore";
import { MCP_PRESETS, presetToServerConfig } from "../../lib/mcp-presets";
import { SearchTab } from "./SearchTab";
import { EmbeddingModelTab } from "./EmbeddingModelTab";
import { LspTab } from "./LspTab";
import { ModelMultiSelect, defaultMakeCaps } from "./ModelMultiSelect";
import { ProviderPicker } from "./ProviderPicker";
import { AddProviderFlow } from "./AddProviderFlow";
import { GlobalCompactModelCard } from "./GlobalCompactModelCard";
import { useTranslation } from "../../i18n/useTranslation";
import { Tooltip } from "../common/Tooltip";
import { ErrorBox } from "../common/ErrorBox";
import { ExpandableRow, ListBox, ListRow } from "../common/list";
import { TabButton } from "../common/tab";

type HarnessTab = "providers" | "search" | "mcp" | "embedding" | "lsp";

export function HarnessPage() {
  const { t } = useTranslation();
  const [activeTab, setActiveTab] = useState<HarnessTab>("providers");

  const tabs: { id: HarnessTab; label: string }[] = [
    { id: "providers", label: t("harness.tabProviders") },
    { id: "search", label: t("harness.tabSearch") },
    { id: "mcp", label: t("harness.tabMcp") },
    { id: "embedding", label: t("harness.tabEmbedding") },
    { id: "lsp", label: t("harnessLsp.tabLsp") },
  ];

  return (
    <div className="flex flex-1 flex-col bg-nav-surface">
      {/* Tabs */}
      <div className="flex gap-1 border-b border-border-divider px-6 pt-2">
        {tabs.map((tab) => (
          <TabButton
            key={tab.id}
            onClick={() => setActiveTab(tab.id)}
            active={activeTab === tab.id}
          >
            {tab.label}
          </TabButton>
        ))}
      </div>

      {/* Tab content — CSS visibility preserves component state across tab switches */}
      <div className="flex-1 overflow-y-auto p-6">
        <div style={{ display: activeTab === "providers" ? "block" : "none" }}><ProvidersTab /></div>
        <div style={{ display: activeTab === "search" ? "block" : "none" }}><SearchTab /></div>
        <div style={{ display: activeTab === "mcp" ? "block" : "none" }}><McpTab /></div>
        <div style={{ display: activeTab === "embedding" ? "block" : "none" }}><EmbeddingModelTab /></div>
        <div style={{ display: activeTab === "lsp" ? "block" : "none" }}><LspTab /></div>
      </div>
    </div>
  );
}

/** Provider configuration */
function ProvidersTab() {
  const { t } = useTranslation();
  const [keys, setKeys] = useState<VaultKeyEntry[]>([]);
  const [keysLoading, setKeysLoading] = useState(true);
  const [showEditDialog, setShowEditDialog] = useState<string | null>(null);

  // AddProviderFlow state
  const [showAddFlow, setShowAddFlow] = useState(false);
  const [addFlowProvider, setAddFlowProvider] = useState<string | undefined>(undefined);
  const [addFlowEntry, setAddFlowEntry] = useState<ProviderListEntry | undefined>(undefined);

  // Edit dialog state
  /** Existing accounts for the provider being edited. `originalAlias`
 *  is snapshotted at dialog open so we can detect a change on save
 *  without re-reading from `keys` (which may shift if other code
 *  refetches). `key` stays empty unless the user wants to rotate;
 *  a non-empty value at save time triggers `update_account_key`.
 *  Entries removed from this list dispatch `remove_key` on save. */
  const [editExistingEntries, setEditExistingEntries] = useState<
    { accountId: string; originalAlias: string; alias: string; key: string }[]
  >([]);
  /** accountIds snapshotted at dialog open. Any id present here but
 *  missing from `editExistingEntries` at save time was removed by
 *  the user via the inline minus button and needs a DELETE call. */
  const [editOriginalAccountIds, setEditOriginalAccountIds] = useState<string[]>([]);
  /** New accounts to add when saving the edit dialog. Each row is
 *  `{alias, key}`; the first row always exists so the UI has at
 *  least one editable input. `ponytail: hard ceiling for UX, lift if needed. */
  const [editKeyEntries, setEditKeyEntries] = useState<
    { alias: string; key: string }[]
  >([{ alias: "", key: "" }]);
  const EDIT_MAX_KEY_ENTRIES = 256;
  const [editBaseUrl, setEditBaseUrl] = useState("");
  const [editModels, setEditModels] = useState<string[]>([]);
  const [editAvailableModels, setEditAvailableModels] = useState<ModelInfo[]>([]);
  const [editModelsLoading, setEditModelsLoading] = useState(false);

  // Edit dialog — per-model capabilities state
  const [editModelCaps, setEditModelCaps] = useState<Record<string, ModelCapabilitiesInfo>>({});
  const [editExpandedModels, setEditExpandedModels] = useState<Set<string>>(new Set());
  const [editCompactModel, setEditCompactModel] = useState("");

  // Tools-tab style level-1 collapsible groups on this tab (default open).
  const [configuredOpen, setConfiguredOpen] = useState(true);

  // Gateway config for default provider indication
  const [config, setConfig] = useState<GatewayConfig | null>(null);

  // Dynamic provider list from Gateway API
  const [dynamicProviders, setDynamicProviders] = useState<ProviderListEntry[]>([]);


  const fetchKeys = useCallback(async () => {
    try {
      const result = await invoke<VaultKeyEntry[]>("list_keys");
      setKeys(result);
    } catch {
      // Gateway may not be running
    } finally {
      setKeysLoading(false);
    }
  }, []);

  const fetchConfig = useCallback(async () => {
    try {
      const resp = await fetch(`${getGatewayUrl()}/api/config`);
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const result = await resp.json() as GatewayConfig;
      setConfig(result);
    } catch {
      // Gateway may not be running
    }
  }, []);

  // Load provider list from Gateway API (offline_providers.json is the sole data source)
  const loadProviders = useCallback(async () => {
    try {
      const response = await fetch(`${getGatewayUrl()}/api/models`);
      if (response.ok) {
        const data = await response.json();
        setDynamicProviders(data.providers ?? []);
      }
    } catch {
      // Gateway may not be running
    }
  }, []);

  useEffect(() => {
    fetchKeys();
    fetchConfig();
    loadProviders();
  }, [fetchKeys, fetchConfig, loadProviders]);

  // Fetch available models for a provider from Gateway API
  const fetchModels = useCallback(async (providerId: string): Promise<ModelInfo[]> => {
    try {
      const data = await fetchProviderModels(providerId);
      return data.models ?? [];
    } catch {
      return [];
    }
  }, []);

  // Group configured keys by provider so the list shows one row per
  // provider with an "N keys" badge, not N near-duplicate rows. Order
  // follows the first appearance of each provider in `keys`.
  const keysByProvider = useMemo(() => {
    const order: string[] = [];
    const map = new Map<string, VaultKeyEntry[]>();
    for (const k of keys) {
      const list = map.get(k.provider);
      if (list) list.push(k);
      else {
        map.set(k.provider, [k]);
        order.push(k.provider);
      }
    }
    return order.map((provider) => ({
      provider,
      accounts: map.get(provider) ?? [],
    }));
  }, [keys]);

  // Remove the entire provider entry — drops every key in one call.
  // Per-account edits/removes still happen inside the edit dialog.
  const handleRemoveProvider = async (provider: string, count: number) => {
    if (!confirm(t("harness.removeProviderConfirm", { provider, count }))) return;
    try {
      await invoke("remove_key", { provider, accountId: undefined });
      await fetchKeys();
    } catch (e) {
      alert(`${t("harness.failedRemoveKey")}: ${e}`);
    }
  };

  // Set a configured provider as the default for the Gateway
  const handleSetDefaultProvider = async (provider: string) => {
    try {
      const entry = keys.find((k) => k.provider === provider);
      await fetch(`${getGatewayUrl()}/api/config`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          default_provider: provider,
          default_model: entry?.models?.[0] || entry?.default_model || undefined,
        }),
      });
      await fetchConfig();
    } catch (e) {
      alert(`${t("harness.failedSetDefault")}: ${e}`);
    }
  };

  const handleEdit = async (provider: string) => {
    const keyEntry = keys.find((k) => k.provider === provider);
    const dynamicProvider = dynamicProviders.find((p) => p.id === provider);
    // Snapshot every existing account for this provider into the dialog
    // state. The user can rename, rotate keys, or remove any of these
    // inline. Originals are kept for change detection on save.
    const providerAccounts = keys.filter(
      (k) => k.provider === provider && k.account_id,
    );
    setEditExistingEntries(
      providerAccounts.map((k) => ({
        accountId: k.account_id as string,
        originalAlias: k.alias || "",
        alias: k.alias || "",
        key: "",
      })),
    );
    setEditOriginalAccountIds(providerAccounts.map((k) => k.account_id as string));
    setEditKeyEntries([{ alias: "", key: "" }]);
    setEditBaseUrl(keyEntry?.base_url ?? dynamicProvider?.api ?? "");
    const configuredModels = keyEntry?.models?.length ? keyEntry.models : keyEntry?.default_model ? [keyEntry.default_model] : [];
    setEditModels(configuredModels);
    setEditCompactModel(keyEntry?.compact_model ?? "");
    setEditModelCaps({});
    setEditExpandedModels(new Set());
    setShowEditDialog(provider);
    // Fetch models from Gateway API (includes input_modalities, context_window, etc.)
    setEditModelsLoading(true);
    const models = await fetchModels(provider);
    setEditAvailableModels(models);
    setEditModelsLoading(false);
    // Initialize per-model caps: prefer stored caps from vault (preserves
    // default_reasoning_effort, supports_reasoning, etc.), fall back to
    // live model data or sensible defaults.
    const storedCaps = keyEntry?.model_capabilities ?? {};
    const caps: Record<string, ModelCapabilitiesInfo> = {};
    for (const modelId of configuredModels) {
      const mi = models.find(m => m.id === modelId);
      const stored = storedCaps[modelId];
      caps[modelId] = stored
        ? { ...defaultMakeCaps(mi), ...stored }
        : defaultMakeCaps(mi);
    }
    setEditModelCaps(caps);
  };

  const handleEditSave = async () => {
    if (!showEditDialog) return;
    try {
      const provider = showEditDialog;

      // 1. Removed accounts: any accountId in the original snapshot that's
      //    no longer in `editExistingEntries` was dropped by the user
      //    via the inline minus button. Dispatch DELETE for each.
      const survivingIds = new Set(editExistingEntries.map((e) => e.accountId));
      for (const accountId of editOriginalAccountIds) {
        if (!survivingIds.has(accountId)) {
          await invoke("remove_key", { provider, accountId });
        }
      }

      // 2. Per-account changes: each surviving entry's alias is
      //    compared to its original; if the user supplied a non-empty
      //    key we treat that as a key rotation. Both go through PATCH
      //    on a single round-trip per changed account.
      for (const entry of editExistingEntries) {
        const aliasChanged = entry.alias.trim() !== entry.originalAlias;
        const keyChanged = entry.key.trim().length > 0;
        if (!aliasChanged && !keyChanged) continue;
        await invoke("update_account_key", {
          provider,
          accountId: entry.accountId,
          alias: aliasChanged ? entry.alias.trim() : null,
          key: keyChanged ? entry.key.trim() : null,
        });
      }

      // 3. New accounts: rows the user appended but doesn't have an
      //    accountId. Empty rows are dropped server-side too, so we
      //    filter here for clarity.
      const newKeys = editKeyEntries
        .map((e) => ({ alias: e.alias.trim(), key: e.key.trim() }))
        .filter((e) => e.key.length > 0);

      const keyEntry = keys.find((k) => k.provider === provider);
      const isLocal = isLocalProvider(provider);
      const isCustom = keyEntry?.custom ?? false;

      // 4. Provider-level config (base_url / models / compact_model /
      //    capabilities) + appending new accounts all land in the same
      //    `update_key` invoke. The Gateway handler applies them
      //    atomically against the in-memory state before releasing
      //    the lock. Always invoked: the Gateway treats undefined
      //    fields as "preserve existing", so a no-op save is cheap and
      //    keeps the call path uniform.
      const updatePayload: Record<string, unknown> = {
        provider,
        keys: newKeys,
        baseUrl: editBaseUrl || undefined,
        defaultModel: undefined,
        models: editModels.length > 0 ? editModels : undefined,
      };
      if ((isLocal || isCustom) && editModels.length > 0 && Object.keys(editModelCaps).length > 0) {
        updatePayload.modelCapabilities = editModelCaps;
      }
      // Include compact_model if set; explicitly null otherwise so the
      // Gateway clears the field when the user empties it.
      if (editCompactModel) {
        updatePayload.compactModel = editCompactModel;
      } else {
        updatePayload.compactModel = null;
      }
      await invoke("update_key", updatePayload);

      setShowEditDialog(null);
      await fetchKeys();
      await fetchConfig();
      window.dispatchEvent(new CustomEvent('models-added'));
    } catch (e) {
      alert(`${t("harness.failedUpdateKey")}: ${e}`);
    }
  };

  return (
    <div className="max-w-2xl space-y-4">
      {/* ADR-056: Global default compact model — lives at the top of the Providers Tab. */}
      <GlobalCompactModelCard
        keys={keys}
        providers={dynamicProviders}
      />

      {/* Configured Providers — Tools-tab level-1 collapsible card:
          chevron + title + count badge in the header, default open;
          configured keys render as unified rows in the inset body. */}
      {keysLoading ? (
        <div className="py-3 text-center text-xs text-text-tertiary">{t("harness.loadingKeys")}</div>
      ) : keys.length > 0 && (
        <ListBox dividers={false}>
          <ExpandableRow
            open={configuredOpen}
            onToggle={() => setConfiguredOpen((v) => !v)}
            title={t("harness.configuredProviders", { count: keysByProvider.length })}
            ariaLabel={t("harness.configuredProviders", { count: keysByProvider.length })}
            bodyClassName="rounded-b-md border-t border-border-divider bg-panel-inset"
          >
            <ListBox variant="plain">
              {keysByProvider.map(({ provider, accounts }) => {
                const providerMeta = dynamicProviders.find((p) => p.id === provider);
                const providerName = providerMeta?.name || provider;
                const isLocal = accounts[0]?.local || isLocalProvider(provider);
                const isCustom = accounts[0]?.custom || providerMeta?.custom;
                const isDefault = config?.default_provider === provider;
                // Per-provider model list lives on every account entry
                // (same value across accounts); surface the first one.
                const first = accounts[0];

                return (
                  <ListRow
                    key={provider}
                    trailing={
                      <div className="flex shrink-0 items-center gap-1.5">
                        <Tooltip content={isDefault ? t("harness.defaultProvider") : t("harness.setDefaultProvider")} variant="plain">
                          <button
                            type="button"
                            onClick={() => handleSetDefaultProvider(provider)}
                            aria-label={t("harness.setDefaultProvider")}
                            className={cn(
                              "rounded p-0.5",
                              isDefault
                                ? "text-amber-500"
                                : "text-text-tertiary hover:text-amber-500 dark:hover:text-amber-400",
                            )}
                          >
                            <Star className="h-3.5 w-3.5" />
                          </button>
                        </Tooltip>
                        <button
                          type="button"
                          onClick={() => handleEdit(provider)}
                          className="rounded btn-solid px-2 py-0.5 text-xs"
                        >
                          {t("harness.edit")}
                        </button>
                        <button
                          type="button"
                          onClick={() => handleRemoveProvider(provider, accounts.length)}
                          className="rounded btn-solid px-2 py-0.5 text-xs"
                        >
                          {t("harness.remove")}
                        </button>
                      </div>
                    }
                  >
                    <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5">
                      <ProviderLogo providerId={provider} size={12} />
                      <span className="truncate text-xs font-medium text-text-secondary ">{providerName}</span>
                      <span className="text-xs" style={{ color: "var(--color-accent)" }}>{t("harness.active")}</span>
                      {isCustom ? (
                        <Tooltip content={t("harness.customProviderNoKey")} variant="plain">
                          <span className="rounded bg-blue-100 px-1.5 py-0.5 text-[10px] text-blue-700 dark:bg-blue-900/30 dark:text-blue-400">
                            🔧 {t("harness.custom")}
                          </span>
                        </Tooltip>
                      ) : isLocal ? (
                        <Tooltip content={t("harness.localProviderNoKey")} variant="plain">
                          <span className="rounded bg-zinc-100 px-1.5 py-0.5 text-[10px] text-text-secondary dark:bg-zinc-700 ">
                            🏠 {t("harness.local")}
                          </span>
                        </Tooltip>
                      ) : (
                        <span className="rounded bg-zinc-100 px-1.5 py-0.5 text-[10px] text-text-secondary dark:bg-zinc-700">
                          {t("harness.keysCount", { count: accounts.length })}
                        </span>
                      )}
                    </div>
                    <div className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-0.5">
                      {first?.models?.length ? (
                        <span className="text-[11px] text-text-tertiary">{first.models.join(", ")}</span>
                      ) : first?.default_model ? (
                        <span className="text-[11px] text-text-tertiary">{first.default_model}</span>
                      ) : (
                        <span className="text-[11px] text-text-tertiary">—</span>
                      )}
                      {first?.compact_model && (
                        <Tooltip content={t("harness.compactModelHint")} variant="plain">
                          <span className="rounded bg-zinc-100 px-1.5 py-0.5 text-[10px] text-text-secondary dark:bg-zinc-700 ">
                            {t("harness.compact")}: {first?.compact_model}
                          </span>
                        </Tooltip>
                      )}
                    </div>
                  </ListRow>
                );
              })}
            </ListBox>
          </ExpandableRow>
        </ListBox>
      )}

      {/* Available Providers — shared ProviderPicker renders three level-1
          collapsible groups (Custom / Local / Remote), each default open.
          No divider lines: boxes are spaced evenly by the root space-y-4. */}
      <ProviderPicker
        providers={dynamicProviders}
        keys={keys}
        onConnect={(providerId, entry) => {
          setAddFlowProvider(providerId);
          setAddFlowEntry(entry);
          setShowAddFlow(true);
        }}
        onAddCustom={() => {
          setAddFlowProvider(undefined);
          setAddFlowEntry(undefined);
          setShowAddFlow(true);
        }}
        onCatalogRefreshed={loadProviders}
      />

      {/* Add Provider Flow dialog (picker → add / custom) */}
      <AddProviderFlow
        open={showAddFlow}
        initialStep={addFlowProvider ? "add" : "custom"}
        initialProvider={addFlowProvider}
        initialProviderEntry={addFlowEntry}
        onClose={() => {
          setShowAddFlow(false);
          setAddFlowProvider(undefined);
          setAddFlowEntry(undefined);
        }}
        onSuccess={async () => {
          await fetchKeys();
          await fetchConfig();
          await loadProviders();
        }}
      />

      {/* Edit key dialog */}
      {showEditDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-modal-overlay">
          <div className="w-[440px] max-h-[85vh] overflow-y-auto rounded-md bg-modal-surface p-6 shadow-xl">
            <h3 className="mb-3 text-sm font-semibold">{t("harness.editProvider")} {showEditDialog}</h3>

            <div className="space-y-2">
              {!isLocalProvider(showEditDialog) && (
                <div>
                  <div className="mb-1 flex items-baseline justify-between">
                    <label className="block text-xs text-text-tertiary">{t("harness.apiKey")}</label>
                    <span className="text-[10px] text-text-tertiary">
                      {t("harness.accountLabel", { defaultValue: "alias" })}
                    </span>
                  </div>
                  <div className="max-h-[200px] space-y-1.5 overflow-y-auto pr-1">
                    {/* Existing accounts — editable inline. Remove from
                        this list drops the account on save. Empty key
                        leaves the stored value untouched; non-empty
                        rotates it. */}
                    {editExistingEntries.map((entry, idx) => (
                      <div key={entry.accountId} className="flex items-center gap-1.5">
                        <StyledInput
                          type="text"
                          value={entry.alias}
                          onChange={(e) => {
                            const next = [...editExistingEntries];
                            next[idx] = { ...next[idx], alias: e.target.value };
                            setEditExistingEntries(next);
                          }}
                          placeholder="alias"
                          className="w-[110px] shrink-0"
                        />
                        <StyledInput
                          type="password"
                          value={entry.key}
                          onChange={(e) => {
                            const next = [...editExistingEntries];
                            next[idx] = { ...next[idx], key: e.target.value };
                            setEditExistingEntries(next);
                          }}
                          placeholder={t("harness.leaveBlankToKeep", {
                            defaultValue: "leave blank to keep",
                          })}
                          className="flex-1"
                        />
                        <button
                          type="button"
                          onClick={() => setEditExistingEntries(editExistingEntries.filter((e) => e.accountId !== entry.accountId))}
                          aria-label="Remove account"
                          className="shrink-0 rounded p-1 text-text-tertiary hover:bg-zinc-100 dark:hover:bg-zinc-700"
                        >
                          <Minus className="h-3.5 w-3.5" />
                        </button>
                      </div>
                    ))}
                    {/* New accounts — appended at the bottom. Disabled
                        when the only row is already empty (otherwise the
                        UI has no editable surface). */}
                    {editKeyEntries.map((entry, idx) => (
                      <div key={`new-${idx}`} className="flex items-center gap-1.5">
                        <StyledInput
                          type="text"
                          value={entry.alias}
                          onChange={(e) => {
                            const next = [...editKeyEntries];
                            next[idx] = { ...next[idx], alias: e.target.value };
                            setEditKeyEntries(next);
                          }}
                          placeholder="alias"
                          className="w-[110px] shrink-0"
                        />
                        <StyledInput
                          type="password"
                          value={entry.key}
                          onChange={(e) => {
                            const next = [...editKeyEntries];
                            next[idx] = { ...next[idx], key: e.target.value };
                            setEditKeyEntries(next);
                          }}
                          placeholder={t("harness.enterNewApiKey")}
                          className="flex-1"
                        />
                        <button
                          type="button"
                          onClick={() => setEditKeyEntries(editKeyEntries.filter((_, i) => i !== idx))}
                          disabled={editKeyEntries.length <= 1}
                          aria-label="Remove key"
                          className="shrink-0 rounded p-1 text-text-tertiary hover:bg-zinc-100 disabled:opacity-30 disabled:hover:bg-transparent dark:hover:bg-zinc-700"
                        >
                          <Minus className="h-3.5 w-3.5" />
                        </button>
                      </div>
                    ))}
                  </div>
                  <button
                    type="button"
                    onClick={() => setEditKeyEntries([...editKeyEntries, { alias: "", key: "" }])}
                    disabled={editKeyEntries.length >= EDIT_MAX_KEY_ENTRIES}
                    className="mt-1.5 flex items-center gap-1 text-xs text-text-tertiary hover:text-text-primary disabled:opacity-30 disabled:hover:text-text-tertiary"
                  >
                    <Plus className="h-3.5 w-3.5" />
                    {t("harness.addKey", { defaultValue: "Add key" })}
                  </button>
                </div>
              )}

              {(
                <div>
                  <label className="mb-1 block text-xs text-text-tertiary">{t("harness.baseUrl")}</label>
                  <StyledInput
                    type="text"
                    value={editBaseUrl}
                    onChange={(e) => setEditBaseUrl(e.target.value)}
                    placeholder="https://..."
                    fontMono
                  />
                </div>
              )}

              {/* Model selection (shared multi-select component) */}
              {(() => {
                const editKeyEntry = showEditDialog ? keys.find(k => k.provider === showEditDialog) : undefined;
                const editIsLocal = showEditDialog ? isLocalProvider(showEditDialog) : false;
                const editIsCustom = editKeyEntry?.custom ?? false;
                return (
                  <ModelMultiSelect
                    models={editAvailableModels}
                    loading={editModelsLoading}
                    selected={editModels}
                    onSelectedChange={setEditModels}
                    caps={editModelCaps}
                    onCapsChange={setEditModelCaps}
                    expandedModels={editExpandedModels}
                    onExpandedToggle={(modelId) =>
                      setEditExpandedModels((prev) => {
                        const next = new Set(prev);
                        if (next.has(modelId)) next.delete(modelId);
                        else next.add(modelId);
                        return next;
                      })
                    }
                    showModelCapEditor={editIsLocal || editIsCustom}
                    compactModel={editCompactModel}
                    onCompactModelChange={setEditCompactModel}
                    showCompactModel={true}
                  />
                );
              })()}
            </div>

            <div className="mt-4 flex items-center justify-end gap-2">
              {/* Buttons with equal width */}
              <button
                onClick={() => setShowEditDialog(null)}
                className="w-20 rounded-md px-3 py-1.5 text-xs font-medium text-center text-text-secondary hover:bg-zinc-100  dark:hover:bg-zinc-700"
              >
                {t("common.cancel")}
              </button>
              <button
                onClick={handleEditSave}
                className="w-20 rounded-md bg-zinc-200 px-3 py-1.5 text-xs font-medium text-center text-text hover:bg-zinc-300 disabled:opacity-50 dark:bg-zinc-700 dark:hover:bg-zinc-600"
              >
                {t("harness.save")}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/** MCP tab — placeholder, content TBD */
const MCP_ICON_MAP: Record<string, React.ComponentType<{ className?: string }>> = {
  Monitor, MousePointer, Search, Globe, BookOpen, FileText, PenTool,
};
const MCP_FALLBACK_ICON = Package;

function McpTab() {
  const { t } = useTranslation();
  const { catalog, loading, error, loadCatalog, addServer, removeServer, probeServer, probeByName,
    installMcp,
    healthStatus, healthErrors, healthToolCounts } = useMcpStore();
  const [showAddForm, setShowAddForm] = useState(false);
  // Tools-tab style level-1 collapsible groups (default open)
  const [catalogOpen, setCatalogOpen] = useState(true);
  const [recommendedOpen, setRecommendedOpen] = useState(true);

  // Probe-before-add state
  const [pendingConfig, setPendingConfig] = useState<McpServerConfigDef | null>(null);
  const [probeResult, setProbeResult] = useState<{ success: boolean; tool_count: number; tools: string[]; error: string | null; duration_ms: number } | null>(null);

  // ADR-072 install dialog state
  const [installRunning, setInstallRunning] = useState(false);
  const [installResult, setInstallResult] = useState<McpInstallRunResponse | null>(null);

  const presetIconMap = useMemo(() => {
    const map: Record<string, string> = {};
    for (const p of MCP_PRESETS) {
      map[p.id] = p.icon ?? "";
    }
    return map;
  }, []);

  // New server form state
  const [newName, setNewName] = useState("");
  const [newTransport, setNewTransport] = useState<McpTransportDef>("stdio");
  const [newCommand, setNewCommand] = useState("");
  const [newArgs, setNewArgs] = useState("");
  const [newUrl, setNewUrl] = useState("");
  const [newEnv, setNewEnv] = useState("");

  // Preset env var form (for servers requiring API keys)
  const [presetEnvForm, setPresetEnvForm] = useState<Record<string, string>>({});
  const [activePreset, setActivePreset] = useState<McpPresetDef | null>(null);

  useEffect(() => {
    loadCatalog();
  }, [loadCatalog]);

  const catalogNames = useMemo(() => new Set(catalog.map((s) => s.name)), [catalog]);

  /** ADR-072: run the install pipeline for a preset (install-then-add). */
  const runPresetInstall = async (preset: McpPresetDef, env: Record<string, string>) => {
    if (!preset.install) return;
    setInstallRunning(true);
    setInstallResult(null);
    const result = await installMcp(preset.id, preset.install, env);
    setInstallRunning(false);
    setInstallResult(result);
  };

  const handleAddFromPreset = async (preset: McpPresetDef) => {
    if (preset.requiredEnv.length > 0) {
      // Show env form for API keys
      setActivePreset(preset);
      setPresetEnvForm(
        preset.requiredEnv.reduce((acc, key) => ({ ...acc, [key]: "" }), {})
      );
      return;
    }
    // No API key needed — install presets go through the install pipeline
    // (ADR-072), everything else keeps the probe-then-add flow.
    if (preset.install) {
      await runPresetInstall(preset, { ...preset.optionalEnv });
      return;
    }
    const config = presetToServerConfig(preset);
    setPendingConfig(config);
    setProbeResult(null);
    const result = await probeServer(config);
    setProbeResult(result);
    if (result.success) {
      addServer(config);
      setPendingConfig(null);
    }
  };

  const handlePresetEnvSubmit = async () => {
    if (!activePreset) return;
    const env = { ...activePreset.optionalEnv, ...presetEnvForm };
    const preset = activePreset;
    setActivePreset(null);
    setPresetEnvForm({});
    if (preset.install) {
      await runPresetInstall(preset, env);
      return;
    }
    const config = presetToServerConfig(preset, env);
    setPendingConfig(config);
    setProbeResult(null);
    const result = await probeServer(config);
    setProbeResult(result);
    if (result.success) {
      addServer(config);
      setPendingConfig(null);
    }
  };

  const handleAddManual = async () => {
    if (!newName.trim()) return;
    const config: McpServerConfigDef = {
      name: newName.trim(),
      transport: newTransport,
      command: newCommand.trim(),
      args: newArgs.trim() ? newArgs.trim().split(/\s+/) : [],
      url: newUrl.trim() || undefined,
      env: newEnv.trim()
        ? Object.fromEntries(
          newEnv.split(",").map((pair) => {
            const [k, ...v] = pair.split("=");
            return [k.trim(), v.join("=").trim()];
          })
        )
        : {},
    };
    setPendingConfig(config);
    setProbeResult(null);
    setShowAddForm(false);
    const result = await probeServer(config);
    setProbeResult(result);
    if (result.success) {
      addServer(config);
      setPendingConfig(null);
      setNewName(""); setNewCommand(""); setNewArgs(""); setNewUrl(""); setNewEnv("");
    }
  };

  /** Add the pending config despite probe failure */
  const handleAddAnyway = () => {
    if (!pendingConfig) return;
    addServer(pendingConfig);
    setPendingConfig(null);
    setProbeResult(null);
  };

  /** Dismiss probe result without adding */
  const dismissProbe = () => {
    setPendingConfig(null);
    setProbeResult(null);
  };

  return (
    <div className="max-w-2xl space-y-4">
      {/* MCP Server Catalog — Tools-tab level-1 collapsible card:
          header carries title + count badge + Add button; server rows
          live in the inset body (default open). */}
      <ListBox dividers={false}>
        <ExpandableRow
          open={catalogOpen}
          onToggle={() => setCatalogOpen((v) => !v)}
          title={t("harnessMcp.mcpServerCatalog", { count: catalog.length })}
          ariaLabel={t("harnessMcp.mcpServerCatalog", { count: catalog.length })}
          trailing={
            <span onClick={(e) => e.stopPropagation()}>
              <button
                type="button"
                onClick={() => setShowAddForm(true)}
                className="inline-flex items-center gap-1 rounded btn-solid px-2 py-1 text-[11px] font-medium"
              >
              {t("harnessMcp.addServer")}
            </button>
          </span>
        }
        bodyClassName="rounded-b-md border-t border-border-divider bg-panel-inset"
      >
          {error && (
            <div className="px-3 pt-2">
              <ErrorBox message={error} onClose={() => useMcpStore.setState({ error: null })} />
            </div>
          )}

          {loading && catalog.length === 0 && (
            <p className="px-3 py-3 text-xs text-text-tertiary">{t("harnessMcp.loadingCatalog")}</p>
          )}

          {!loading && catalog.length === 0 && (
            <p className="px-3 py-3 text-xs text-text-tertiary">
              {t("harnessMcp.noMcpServers")}
            </p>
          )}

          {/* Server list */}
          {catalog.length > 0 && (
            <ListBox variant="plain">
              {catalog.map((server) => {
                const status = healthStatus[server.name];
                const healthErr = healthErrors[server.name];
                const toolCount = healthToolCounts[server.name];
                return (
                  <ListRow
                    key={server.name}
                    trailing={
                      <div className="flex shrink-0 items-center gap-1.5">
                        <button
                          type="button"
                          onClick={() => probeByName(server.name)}
                          disabled={status === "probing"}
                          className="inline-flex items-center gap-1 rounded btn-solid px-2 py-1 text-[11px] font-medium disabled:opacity-50"
                        >
                          {status === "probing" ? "..." : t("harnessMcp.testConn")}
                        </button>
                        <button
                          type="button"
                          onClick={() => removeServer(server.name)}
                          className="inline-flex items-center gap-1 rounded btn-solid px-2 py-1 text-[11px] font-medium"
                        >
                          {t("harnessMcp.remove")}
                        </button>
                      </div>
                    }
                  >
                    <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5">
                      {/* Health indicator dot */}
                      {status === "probing" && (
                        <span className="h-2 w-2 shrink-0 rounded-full bg-amber-400 animate-pulse" title={t("harnessMcp.testing")} />
                      )}
                      {status === "healthy" && (
                        <span className="h-2 w-2 shrink-0 rounded-full bg-green-500" title={t("harnessMcp.connected", { count: toolCount })} />
                      )}
                      {status === "unhealthy" && (
                        <span className="h-2 w-2 shrink-0 rounded-full bg-red-500" title={healthErr || t("harnessMcp.connFailed")} />
                      )}
                      <span className="rounded bg-zinc-100 px-1.5 py-0.5 text-[10px] font-mono text-text-tertiary dark:bg-zinc-700">
                        {server.transport}
                      </span>
                      {(() => {
                        const iconName = presetIconMap[server.name];
                        const Icon = (iconName && MCP_ICON_MAP[iconName]) || MCP_FALLBACK_ICON;
                        return <Icon className="h-3.5 w-3.5 shrink-0 text-text-tertiary" />;
                      })()}
                      <span className="truncate text-xs font-medium text-text-secondary ">{server.name}</span>
                      {server.has_secrets && (
                        <span className="text-[10px] text-amber-500 shrink-0">{t("harnessMcp.hasApiKey")}</span>
                      )}
                      {status === "healthy" && toolCount > 0 && (
                        <span className="text-[10px] text-green-500 shrink-0">{toolCount} tools</span>
                      )}
                    </div>
                    {(server.command || server.url) && (
                      <p className="mt-0.5 truncate text-[10px] text-text-tertiary break-all">
                        {server.command || server.url}
                      </p>
                    )}
                    {/* Show health error inline */}
                    {status === "unhealthy" && healthErr && (
                      <div className="mt-0.5">
                        <ErrorBox message={healthErr} className="!p-2 !text-[10px]" />
                      </div>
                    )}
                  </ListRow>
                );
              })}
            </ListBox>
          )}
        </ExpandableRow>
      </ListBox>

      {/* Recommended servers — Tools-tab level-1 collapsible card: presets
          now render as unified ListRow rows (hairline separators + inset
          hover) instead of the old 2-column mini-card grid. */}
      <ListBox dividers={false}>
        <ExpandableRow
          open={recommendedOpen}
          onToggle={() => setRecommendedOpen((v) => !v)}
          title={t("harnessMcp.recommendedMcpServers", { count: MCP_PRESETS.length })}
          ariaLabel={t("harnessMcp.recommendedMcpServers", { count: MCP_PRESETS.length })}
          bodyClassName="rounded-b-md border-t border-border-divider bg-panel-inset"
        >
          <ListBox variant="plain">
            {MCP_PRESETS.map((preset) => {
              const isInstalled = catalogNames.has(preset.id);
              return (
                <ListRow
                  key={preset.id}
                  surface="inset"
                  leading={
                    (() => {
                      const Icon = MCP_ICON_MAP[preset.icon ?? ""] || MCP_FALLBACK_ICON;
                      return (
                        <Icon className="h-4 w-4 shrink-0 text-text-tertiary " />
                      );
                    })()
                  }
                  trailing={
                    isInstalled ? (
                      <span className="inline-flex shrink-0 items-center gap-1 rounded bg-green-100 px-2 py-1 text-[11px] font-medium text-green-700 dark:bg-green-900/30 dark:text-green-400">
                        <CheckCircle2 className="h-3 w-3" />
                        {t("harnessMcp.installed")}
                      </span>
                    ) : (
                      <button
                        type="button"
                        onClick={() => handleAddFromPreset(preset)}
                        className="inline-flex shrink-0 items-center gap-1 rounded btn-solid px-2 py-1 text-[11px] font-medium"
                      >
                        {preset.install ? (
                          <>
                            <Download className="h-3 w-3" />
                            {t("harnessMcp.install")}
                          </>
                        ) : (
                          <>
                            <Plus className="h-3 w-3" />
                            {t("harnessMcp.add")}
                          </>
                        )}
                      </button>
                    )
                  }
                >
                  <div className="flex flex-wrap items-center gap-x-1.5 gap-y-0.5">
                    <span className="truncate text-xs font-medium text-text-secondary ">{preset.name}</span>
                    <span className="shrink-0 rounded bg-zinc-100 px-1 py-0.5 text-[10px] text-text-tertiary dark:bg-zinc-700">
                      {preset.category}
                    </span>
                  </div>
                  <p className="mt-0.5 line-clamp-1 text-[10px] text-text-tertiary">{preset.description}</p>
                  {preset.requiredEnv.length > 0 && !isInstalled && (
                    <p className="mt-0.5 text-[10px] text-amber-500">
                      {t("harnessMcp.requires")}{preset.requiredEnv.join(", ")}
                    </p>
                  )}
                </ListRow>
              );
            })}
          </ListBox>
        </ExpandableRow>
      </ListBox>

      {/* Add Server dialog */}
      {showAddForm && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-modal-overlay">
          <div className="w-[440px] max-h-[85vh] overflow-y-auto rounded-md bg-modal-surface p-6 shadow-xl">
            <h3 className="mb-3 text-sm font-semibold">{t("harnessMcp.addCustomMcpServer")}</h3>
            <div className="space-y-2">
              <div>
                <label className="mb-1 block text-xs text-text-tertiary">{t("harnessMcp.name")}</label>
                <input
                  value={newName}
                  onChange={(e) => setNewName(e.target.value)}
                  className={inputBase}
                  placeholder="my-server"
                />
              </div>
              <div>
                <label className="mb-1 block text-xs text-text-tertiary">{t("harnessMcp.transport")}</label>
                <Dropdown
                  value={newTransport}
                  onChange={(v) => setNewTransport(v as McpTransportDef)}
                  options={[
                    { value: "stdio", label: "stdio" },
                    { value: "http", label: "http" },
                    { value: "sse", label: "sse" },
                  ]}
                />
              </div>
              {newTransport === "stdio" ? (
                <>
                  <div>
                    <label className="mb-1 block text-xs text-text-tertiary">{t("harnessMcp.command")}</label>
                    <input
                      value={newCommand}
                      onChange={(e) => setNewCommand(e.target.value)}
                      className={inputBase}
                      placeholder="npx"
                    />
                  </div>
                  <div>
                    <label className="mb-1 block text-xs text-text-tertiary">{t("harnessMcp.arguments")}</label>
                    <input
                      value={newArgs}
                      onChange={(e) => setNewArgs(e.target.value)}
                      className={inputBase}
                      placeholder="-y @modelcontextprotocol/server-filesystem"
                    />
                  </div>
                </>
              ) : (
                <div>
                  <label className="mb-1 block text-xs text-text-tertiary">{t("harnessMcp.url")}</label>
                  <input
                    value={newUrl}
                    onChange={(e) => setNewUrl(e.target.value)}
                    className={inputBase}
                    placeholder="http://localhost:3000"
                  />
                </div>
              )}
              <div>
                <label className="mb-1 block text-xs text-text-tertiary">{t("harnessMcp.environment")}</label>
                <input
                  value={newEnv}
                  onChange={(e) => setNewEnv(e.target.value)}
                  className={inputBase}
                  placeholder="API_KEY=sk-xxx, DEBUG=true"
                />
              </div>
            </div>
            <div className="mt-4 flex justify-end gap-2">
              <button
                onClick={() => { setShowAddForm(false); }}
                className="inline-flex items-center gap-1 rounded-md border border-zinc-300 px-3 py-1.5 text-xs font-medium text-text-secondary hover:bg-zinc-50 dark:border-zinc-600  dark:hover:bg-zinc-700"
              >
                {t("common.cancel")}
              </button>
              <button
                onClick={handleAddManual}
                disabled={!newName.trim()}
                className="inline-flex items-center gap-1 rounded btn-accent px-3 py-1.5 text-xs font-medium disabled:opacity-50"
              >
                {t("harnessMcp.addServer")}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Preset env form (for servers requiring API keys) */}
      {activePreset && (
        <div className="rounded-md border border-[var(--color-accent)]/40 bg-modal-surface p-4">
          <h2 className="text-xs font-medium mb-1">{t("harnessMcp.configure")}{activePreset.name}</h2>
          <p className="text-[10px] text-text-tertiary mb-3">{activePreset.installHint}</p>
          <div className="space-y-2">
            {activePreset.requiredEnv.map((envKey) => (
              <div key={envKey}>
                <label className="mb-1 block text-[10px] text-text-tertiary">{envKey}</label>
                <input
                  type="password"
                  value={presetEnvForm[envKey] || ""}
                  onChange={(e) =>
                    setPresetEnvForm((prev) => ({ ...prev, [envKey]: e.target.value }))
                  }
                  className={inputBase}
                  placeholder={`${t("harnessMcp.enter")}${envKey}`}
                />
              </div>
            ))}
            <div className="flex gap-2">
              <button
                onClick={handlePresetEnvSubmit}
                className="inline-flex items-center gap-1 rounded btn-accent px-3 py-1 text-xs font-medium"
              >
                {t("harnessMcp.addServer")}
              </button>
              <button
                onClick={() => { setActivePreset(null); setPresetEnvForm({}); }}
                className="inline-flex items-center gap-1 rounded-md border border-zinc-300 px-3 py-1 text-xs font-medium text-text-secondary hover:bg-zinc-50 dark:border-zinc-600  dark:hover:bg-zinc-700"
              >
                {t("common.cancel")}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Probe result dialog */}
      {probeResult && pendingConfig && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-modal-overlay">
          <div className="w-[400px] rounded-md bg-modal-surface p-6 shadow-xl">
            {probeResult.success ? (
              <>
                <div className="flex items-center gap-2 mb-3">
                  <span className="h-3 w-3 rounded-full bg-green-500" />
                  <h3 className="text-sm font-semibold text-green-600 dark:text-green-400">
                    {t("harnessMcp.connected", { count: probeResult.tool_count })}
                  </h3>
                </div>
                {probeResult.tools.length > 0 && (
                  <div className="mb-3 max-h-32 overflow-y-auto rounded bg-zinc-50 p-2 dark:bg-zinc-700/50">
                    {probeResult.tools.map((tool) => (
                      <span key={tool} className="mr-1 mb-1 inline-block rounded bg-zinc-100 px-1.5 py-0.5 text-[10px] font-mono text-text-secondary dark:bg-zinc-600 ">
                        {tool}
                      </span>
                    ))}
                  </div>
                )}
                <p className="text-[10px] text-text-tertiary mb-3">{probeResult.duration_ms}ms</p>
                <button
                  onClick={dismissProbe}
                  className="inline-flex items-center gap-1 rounded btn-accent px-3 py-1.5 text-xs font-medium"
                >
                  OK
                </button>
              </>
            ) : (
              <>
                <div className="flex items-center gap-2 mb-3">
                  <span className="h-3 w-3 rounded-full bg-red-500" />
                  <h3 className="text-sm font-semibold text-red-600 dark:text-red-400">
                    {t("harnessMcp.connFailed")}
                  </h3>
                </div>
                <div className="mb-3">
                  <ErrorBox
                    message={t("harnessMcp.connFailed")}
                    details={probeResult.error ?? undefined}
                  />
                </div>
                <p className="text-[10px] text-text-tertiary mb-3">{probeResult.duration_ms}ms</p>
                <div className="flex gap-2">
                  <button
                    onClick={handleAddAnyway}
                    className="inline-flex items-center gap-1 rounded-md border border-amber-400 px-3 py-1.5 text-xs font-medium text-amber-600 hover:bg-amber-50 dark:border-amber-600 dark:text-amber-400 dark:hover:bg-amber-900/20"
                  >
                    {t("harnessMcp.addAnyway")}
                  </button>
                  <button
                    onClick={dismissProbe}
                    className="inline-flex items-center gap-1 rounded-md border border-zinc-300 px-3 py-1.5 text-xs font-medium text-text-secondary hover:bg-zinc-50 dark:border-zinc-600  dark:hover:bg-zinc-700"
                  >
                    {t("common.cancel")}
                  </button>
                </div>
              </>
            )}
          </div>
        </div>
      )}

      {/* Probing spinner overlay (shown while probe is in progress) */}
      {pendingConfig && !probeResult && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-modal-overlay">
          <div className="w-[300px] rounded-md bg-modal-surface p-6 text-center shadow-xl">
            <div className="mx-auto mb-3 h-8 w-8 animate-spin rounded-full border-2 border-zinc-300 border-t-[var(--color-accent)]" />
            <p className="text-xs text-text-tertiary">{t("harnessMcp.testing")}</p>
            <p className="mt-1 text-[10px] text-text-tertiary">{pendingConfig.name}</p>
          </div>
        </div>
      )}

      {/* ADR-072 install dialog (running / result / guidance) */}
      {(installRunning || installResult) && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-modal-overlay">
          <div className="w-[520px] max-h-[85vh] overflow-y-auto rounded-md bg-modal-surface p-6 shadow-xl">
            {installRunning ? (
              <>
                <div className="flex items-center gap-2 mb-3">
                  <Loader2 className="h-4 w-4 animate-spin text-[var(--color-accent)]" />
                  <h3 className="text-sm font-semibold">{t("harnessMcp.installing")}</h3>
                </div>
                <p className="text-xs text-text-tertiary">
                  {t("harnessMcp.installFirstRunHint")}
                </p>
              </>
            ) : installResult?.success ? (
              <>
                <div className="flex items-center gap-2 mb-3">
                  <CheckCircle2 className="h-4 w-4 text-green-500" />
                  <h3 className="text-sm font-semibold text-green-600 dark:text-green-400">
                    {t("harnessMcp.installSuccess", { count: installResult.tool_count ?? 0 })}
                  </h3>
                </div>
                {installResult.stdout && (
                  <pre className="mb-3 max-h-48 overflow-y-auto rounded bg-zinc-50 p-2 text-[10px] text-text-secondary dark:bg-zinc-700/50 ">
                    {installResult.stdout}
                  </pre>
                )}
              </>
            ) : (
              <>
                <div className="flex items-center gap-2 mb-3">
                  <XCircle className="h-4 w-4 text-red-500" />
                  <h3 className="text-sm font-semibold text-red-600 dark:text-red-400">
                    {t("harnessMcp.installFailed")}
                  </h3>
                </div>
                <ErrorBox
                  message={t("harnessMcp.installFailed")}
                  details={installResult?.stderr || installResult?.health_error || undefined}
                />
                {(installResult?.stderr || installResult?.health_error) && (
                  <pre className="mt-3 max-h-48 overflow-y-auto rounded bg-zinc-50 p-2 text-[10px] text-red-500 dark:bg-zinc-700/50 dark:text-red-400">
                    {installResult?.health_error
                      ? `${installResult.stderr}\n\n[health check] ${installResult.health_error}`
                      : installResult?.stderr}
                  </pre>
                )}
              </>
            )}
            <div className="mt-4 flex justify-end">
              <button
                onClick={() => { setInstallResult(null); setInstallRunning(false); }}
                className="inline-flex items-center gap-1 rounded btn-accent px-3 py-1.5 text-xs font-medium"
              >
                OK
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/** GatewayConfig type for local usage */
interface GatewayConfig {
  default_provider?: string;
  default_model?: string;
}