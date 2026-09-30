import { Bot } from "lucide-react";
import { useTranslation } from "../../i18n/useTranslation";
import { cn } from "../../lib/utils";
import { CAPSULE_PANE_CN } from "../common/capsule";

/**
 * EmptyChatPane — the middle column when no agent and no peer is selected.
 *
 * This used to be a bare `flex-1` div of centered text, which left the
 * whole right side of the window as undifferentiated vibrancy glass: with
 * no agent installed, only the AgentList capsule was outlined and the app
 * looked like a half-rendered frame. It now wears the same capsule shell as
 * the live chat view (CAPSULE_PANE_CN + bg-chat-body) so the layout and its
 * surface paint from the first frame, and the transition into a real
 * session has no transparent gap to fill in.
 */
export function EmptyChatPane() {
  const { t } = useTranslation();
  return (
    <div className={cn(CAPSULE_PANE_CN, "min-w-[288px] flex-1 bg-chat-body")}>
      <div className="flex flex-1 flex-col items-center justify-center gap-3 p-8 text-center">
        <Bot className="h-10 w-10 text-text-tertiary" aria-hidden="true" />
        <p className="text-xs text-text-tertiary">{t("chatPanel.selectAgentOrPeer")}</p>
      </div>
    </div>
  );
}

/**
 * EmptyRightPane — the right column's capsule placeholder, sized to the
 * width RightPanel would occupy.
 *
 * Purely a frame: the workspace / status / memory tabs are all agent-scoped
 * (their data comes from the selected agent's Runtime), so there is nothing
 * truthful to render inside — but reserving the outlined slot keeps the
 * four-capsule layout intact instead of bleeding the chat pane to the
 * window edge whenever nothing is selected. Rendered only when
 * `rightPanelCollapsed` is false, so a deliberately collapsed panel still
 * gives the width back.
 */
export function EmptyRightPane({ width }: { width: number }) {
  return (
    <div
      className="relative flex shrink-0"
      style={{ width }}
      aria-hidden="true"
    >
      <div className={cn(CAPSULE_PANE_CN, "flex-1 bg-right-panel")} />
    </div>
  );
}
