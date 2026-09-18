import { Globe, Lock } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useAgentStore } from "../../stores/agentStore";
import { useToast } from "../common/ToastProvider";
import { Tooltip } from "../common/Tooltip";
import { toolbarButton } from "../../lib/ui-styles";

/**
 * ADR-076 §决策 4: per-session read-visibility toggle, mounted in the
 * composer toolbar next to the other session-scoped controls.
 *
 * Public = readable by every signed-in account; private = owner + admins
 * only. It is a **read**-side setting, so it lives on the session (not on
 * the agent, which is a Gateway-wide shared object) and only the owner may
 * flip it — `can_write === false` renders the icon disabled rather than
 * hiding it, so a viewer can still see what they are looking at.
 *
 * The click is optimistic (`agentStore.setSessionVisibility` patches the
 * session row, then PUTs) and is NOT behind a confirmation dialog: the
 * change is reversible, low-stakes, and the backend re-reports the truth
 * on the next `fetchSessions`.
 */
export function SessionVisibilityToggle({
  agentId,
  sessionId,
}: {
  agentId: string;
  sessionId: string;
}) {
  const { t } = useTranslation();
  const { addToast } = useToast();
  // Selecting the row object itself (not a derived boolean) keeps this
  // re-rendering only when `renameSession` / `setSessionVisibility` swap
  // the object — same read path as ChatPanel's `readOnlySession`.
  const session = useAgentStore((s) =>
    s.agents[agentId]?.sessions.find((x) => x.session_id === sessionId),
  );

  // Absent on responses from an older Runtime → treat as public (the
  // on-disk default) instead of hiding the control.
  const isPrivate = session?.visibility === "private";
  const canWrite = session?.can_write !== false;

  const label = isPrivate
    ? t("sessionVisibility.private")
    : t("sessionVisibility.public");

  const handleToggle = async () => {
    const next = isPrivate ? "public" : "private";
    try {
      await useAgentStore.getState().setSessionVisibility(agentId, sessionId, next);
      addToast({
        type: "success",
        message: t(
          next === "private"
            ? "sessionVisibility.toastNowPrivate"
            : "sessionVisibility.toastNowPublic",
        ),
      });
    } catch (e) {
      addToast({
        type: "error",
        message: t("sessionVisibility.toastFailed", {
          error: e instanceof Error ? e.message : String(e),
        }),
      });
    }
  };

  return (
    <Tooltip
      content={
        canWrite ? `${label} — ${t("sessionVisibility.hintToggle")}` : t("chatPanel.readOnlySession")
      }
    >
      <button
        className={toolbarButton}
        onClick={handleToggle}
        disabled={!canWrite}
        aria-label={label}
        aria-pressed={isPrivate}
      >
        {isPrivate ? <Lock size={14} /> : <Globe size={14} />}
      </button>
    </Tooltip>
  );
}
