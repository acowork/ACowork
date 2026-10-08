import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "../i18n/useTranslation";
import { useAuthStore } from "../stores/authStore";
import { useSettingsStore } from "../stores/settingsStore";
import { useGatewayStore } from "../stores/gatewayStore";
import { useAgentStore } from "../stores/agentStore";
import { useToast } from "../components/common/ToastProvider";
import { log } from "../lib/logger";

/** Shape of `create_local_node`'s success payload (see src-tauri). */
interface CreateLocalNodeResult {
  pid?: number;
  was_enrolled?: boolean;
  already_running?: boolean;
  skipped_not_enrolled?: boolean;
}

/**
 * ADR-087 follow-up: in remote Gateway mode, resume this machine's local
 * Node automatically on every Desktop launch — the same guarantee Local
 * mode already gives (the Gateway auto-spawns its node there), extended
 * to Desktop→remote-Gateway topologies.
 *
 * Scope of the silent path (deliberate):
 *   - Only RESUMES an existing enrollment. First-time enrollment mints an
 *     account-bound token and leaves a persistent daemon on the machine —
 *     that stays an explicit user action (AgentList banner / menu item),
 *     so the Rust command is called with `silent: true` and returns
 *     `skipped_not_enrolled` on an unenrolled machine.
 *   - Success is silent (no toast); only spawn failures surface a toast.
 *   - Runs once per (account, gateway URL) per app session — switching
 *     Gateway or re-logging in re-arms it, a Gateway flap does not.
 */
export function useLocalNodeAutoStart(): void {
  const account = useAuthStore((s) => s.account);
  const gatewayMode = useSettingsStore((s) => s.gatewayMode);
  const gatewayUrl = useSettingsStore((s) => s.gatewayUrl);
  const autoStart = useSettingsStore((s) => s.autoStartLocalNode);
  const status = useGatewayStore((s) => s.status);
  const { t } = useTranslation();
  const { addToast } = useToast();

  // Session-scoped dedup key set (component remounts keep it via ref on
  // the AppLayout owner; a full reload resets it, which is intended).
  const fired = useRef<Set<string>>(new Set());

  useEffect(() => {
    if (!account || gatewayMode !== "remote" || !autoStart || status !== "connected") {
      return;
    }
    const key = `${account.user_id}|${gatewayUrl}`;
    if (fired.current.has(key)) return;
    fired.current.add(key);

    void (async () => {
      try {
        const res = await invoke<CreateLocalNodeResult>("create_local_node", { silent: true });
        if (res.already_running || res.skipped_not_enrolled) return;
        // A fresh spawn landed — refresh the node list so the sidebar
        // groups pick it up without a manual retry.
        log.info("[localNode] auto-started local node", { pid: res.pid });
        await useAgentStore.getState().fetchNodes();
      } catch (e) {
        // Failure is worth one toast (no modal): the user can still start
        // the node manually from the add-menu.
        log.warn("[localNode] auto-start failed:", e);
        addToast({ type: "error", message: t("agentList.errorCreateLocalNode", { error: String(e) }) });
      }
    })();
  }, [account, gatewayMode, gatewayUrl, autoStart, status, addToast, t]);
}
