import { useCallback, useEffect, useMemo, useState } from "react";
import type {
  VaultKeyEntry,
  ProviderListEntry,
  CompactModelRef,
} from "../../lib/types";
import { getDefaultCompactModels, setDefaultCompactModels } from "../../lib/gateway-api";
import { ModelPriorityList } from "../common/ModelPriorityList";
import { cn } from "../../lib/utils";
import { useTranslation } from "../../i18n/useTranslation";
import { ExpandableRow, ListBox } from "../common/list";
import { useToast } from "../common/ToastProvider";

export interface GlobalCompactModelCardProps {
  /** Configured providers (used to build the option list). */
  keys: VaultKeyEntry[];
  /** Available provider entries (name, model_count). */
  providers: ProviderListEntry[];
}

/** One entry in the dropdown — stable order, deduped by `provider::model`. */
export interface CompactModelOption {
  key: string;
  providerId: string;
  modelId: string;
}

/** Build (provider, model) options from configured keys, in stable order.
 *
 *  Data model reminder: `keys` is a flat list of provider accounts.
 *  A provider (e.g. deepseek) can have N accounts (N = number of API
 *  keys the user added); every account for that provider shares the
 *  SAME `models` array — model catalog is a property of the provider,
 *  not the account. So iterating `keys` naively emits the same
 *  `provider::model` key once per account (e.g. 2 deepseek accounts
 *  with `models: ["deepseek-flash"]` → 2× "deepseek::deepseek-flash"
 *  options), which makes React warn and reconcile-loop.
 *
 *  This picker operates on the (provider, model) axis only — the
 *  account dimension is irrelevant at this level (Runtime handles
 *  account routing when the request actually fires). So dedupe by
 *  `${provider}::${model}` and keep first occurrence's order. */
export function buildCompactModelOptions(
  keys: VaultKeyEntry[],
): CompactModelOption[] {
  const out: CompactModelOption[] = [];
  const seen = new Set<string>();
  for (const k of keys) {
    const modelIds =
      k.models && k.models.length > 0
        ? k.models
        : k.default_model
          ? [k.default_model]
          : [];
    for (const modelId of modelIds) {
      const key = `${k.provider}::${modelId}`;
      if (seen.has(key)) continue;
      seen.add(key);
      out.push({ key, providerId: k.provider, modelId });
    }
  }
  return out;
}

/**
 * "Global default compact model" — top of the Providers Tab.
 *
 * Holds a `provider_id::model_id` pick at the `provider_list.json` top
 * level (`default_compact_model`), independent of any single provider's
 * `compact_model`. Persistence:
 *
 *   PUT /api/settings/default-compact-model  →  Gateway writes provider_list.json
 *                                            →  MQTT republish triggers
 *                                            →  Runtimes refresh AgentCore.default_compact_model
 *
 * UX: an ordered list editor (add / move up-down / remove). Every
 * mutation PUTs the full list (optimistic update, rollback on error);
 * an empty list clears the setting.
 */
export function GlobalCompactModelCard({
  keys,
  providers,
}: GlobalCompactModelCardProps) {
  const { t } = useTranslation();
  const { addToast } = useToast();

  const [current, setCurrent] = useState<CompactModelRef[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  // Fold state — default open, same convention as the other collapsible
  // cards on this page (Configured Providers, Embedding Service Status).
  const [open, setOpen] = useState(true);

  const refresh = useCallback(async () => {
    try {
      const v = await getDefaultCompactModels();
      setCurrent(v);
    } catch {
      // Gateway may be down — leave the list empty, UI stays editable
      setCurrent([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // Build (provider, model) options from configured keys, in stable order.
  const options = useMemo(() => buildCompactModelOptions(keys), [keys]);

  const providerNameById = useMemo(() => {
    const m = new Map<string, string>();
    for (const p of providers) m.set(p.id, p.name);
    return m;
  }, [providers]);

  // Every mutation PUTs the full ordered list (optimistic update,
  // rollback on error). The Gateway validates atomically — a rejected
  // list leaves the persisted one untouched.
  const handleChange = async (next: CompactModelRef[]) => {
    if (JSON.stringify(next) === JSON.stringify(current)) return;
    const previous = current;
    setCurrent(next);
    setSaving(true);
    try {
      const persisted = await setDefaultCompactModels(next);
      setCurrent(persisted);
    } catch (e) {
      setCurrent(previous);
      addToast({
        type: "error",
        message: t("harness.globalCompactModel.saveFailed", {
          error: e instanceof Error ? e.message : String(e),
        }),
      });
    } finally {
      setSaving(false);
    }
  };

  return (
    <ListBox dividers={false}>
      <ExpandableRow
        open={open}
        onToggle={() => setOpen((v) => !v)}
        title={t("harness.globalCompactModel.title")}
        ariaLabel={t("harness.globalCompactModel.title")}
        bodyClassName="rounded-b-md border-t border-border-divider bg-panel-inset p-3"
      >
        <div className="space-y-2">
          <p className="text-11 text-text-tertiary ">
            {t("harness.globalCompactModel.description")}
          </p>
          <ModelPriorityList
            className={cn(saving && "opacity-60")}
            items={current}
            options={options}
            providerNameById={providerNameById}
            disabled={loading || saving}
            onChange={(next) => void handleChange(next)}
          />
        </div>
      </ExpandableRow>
    </ListBox>
  );
}