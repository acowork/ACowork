/**
 * Shared message-row primitives for the user↔user inbox thread.
 *
 * Extracted from the original (now-deleted) MessagesView so the inbox
 * middle panel ([InboxPanel](./InboxPanel.tsx)) can render the same bubble /
 * attachment visuals without duplicating ~150 lines.
 */

import { useEffect, useState } from "react";
import { Download, FileText } from "lucide-react";

import { useTranslation } from "../i18n/useTranslation";
import {
  attachmentObjectUrl,
  downloadChatAttachment,
  formatAttachmentSize,
} from "../lib/user-chat-api";
import { formatBubbleTime } from "../lib/formatTime";
import { cn } from "../lib/utils";
import type { ChatAttachment, UserChatMessage } from "../lib/types";
import { UserAvatar } from "../components/common/UserAvatar";

export function MessageRow({
  message,
  own,
  youLabel,
  userId,
  chatId,
  ownAvatarUrl,
  ownBuiltinAvatarId,
  ownDisplayName,
  peerAvatarUrl,
  peerBuiltinAvatarId,
  peerDisplayName,
}: {
  message: UserChatMessage;
  own: boolean;
  youLabel: string;
  /** Signed-in account, for building attachment URLs (token goes via fetch). */
  userId: string;
  chatId: string;
  /** Self avatar (own row). Source = `useAuthStore.account.{avatar, builtin_avatar}`. */
  ownAvatarUrl?: string | null;
  ownBuiltinAvatarId?: string | null;
  ownDisplayName: string;
  /** Peer avatar (peer row). Source = the `ChatSummary` matched to `message.from`. */
  peerAvatarUrl?: string | null;
  peerBuiltinAvatarId?: string | null;
  peerDisplayName: string;
}) {
  const { t } = useTranslation();
  const attachments = message.attachments ?? [];
  const hasBody = message.body.trim().length > 0;
  const ts = formatBubbleTime(message.ts * 1000);

  // Layout is a 1:1 mirror of
  // [UserWithAttachmentsBubble](../components/chat/UserWithAttachmentsBubble.tsx)
  // and ChatPanel's `MessageBubble` user branch. Every class on every
  // wrapper matches agent — do NOT diverge from the agent structure
  // even by one token (the layout collapse bug ("two English words per
  // line") came from reordering wrappers around max-w).
  //
  // Width — `max-w-[var(--content-max-width)]` follows the global
  // content-width setting (settingsStore → `applyContentWidth` → CSS
  // variable on `<html>`, default 80% per `lib/defaults.ts`). The agent
  // assistant branch uses the same variable.
  const bubbleClass = own
    ? "rounded-md rounded-br-sm bg-chat-user text-chat-user-text select-text"
    : "rounded-md rounded-bl-sm bg-chat-bubble text-text-primary select-text";
  const senderName = own ? youLabel : peerDisplayName;
  const avatar = own ? (
    <UserAvatar
      displayName={ownDisplayName}
      avatarUrl={ownAvatarUrl ?? null}
      builtinAvatarId={ownBuiltinAvatarId ?? null}
      size={40}
      className="shrink-0 mt-1"
    />
  ) : (
    <UserAvatar
      displayName={peerDisplayName}
      avatarUrl={peerAvatarUrl ?? null}
      builtinAvatarId={peerBuiltinAvatarId ?? null}
      size={40}
      className="shrink-0 mt-1"
    />
  );

  return (
    <div
      className={cn(
        "mb-2 flex items-start gap-2",
        own ? "justify-end" : "justify-start",
      )}
    >
      {!own && avatar}
      {/* Content column — mirrors `min-w-0 flex-1 flex flex-col items-end`
          in agent's user branch. */}
      <div
        className={cn(
          "min-w-0 flex-1 flex flex-col",
          own ? "items-end" : "items-start",
        )}
      >
        {senderName && (
          <span className="mt-[2px] text-xs text-text-tertiary">{senderName}</span>
        )}
        {/* Bubble wrapper — mirrors `group mt-[6px] max-w-[85%] flex flex-col
            items-start` from agent user branch. Keep `flex flex-col items-start`
            verbatim — the timestamp sibling needs that column to align its
            left edge to the bubble's left edge. */}
        <div className="group mt-[6px] max-w-[var(--content-max-width)] flex flex-col items-start">
          <div
            className={cn(
              "w-full rounded-md max-h-48 overflow-y-auto",
              bubbleClass,
            )}
            style={{ fontSize: "var(--ui-font-size, 0.875rem)" }}
          >
            <div className="px-4 py-2.5 whitespace-pre-wrap break-words">
              {attachments.length > 0 && (
                <div className={cn("flex flex-col gap-1", hasBody && "mb-1")}>
                  {attachments.map((attachment) =>
                    attachment.mime.startsWith("image/") ? (
                      <ImageAttachment
                        key={attachment.id}
                        userId={userId}
                        chatId={chatId}
                        attachment={attachment}
                        downloadLabel={t("messages.download")}
                      />
                    ) : (
                      <FileAttachment
                        key={attachment.id}
                        userId={userId}
                        chatId={chatId}
                        attachment={attachment}
                      />
                    ),
                  )}
                </div>
              )}
              {hasBody && <div>{message.body}</div>}
            </div>
          </div>
          <span className="mt-1 text-[10px] text-text-tertiary opacity-0 transition-opacity group-hover:opacity-100">
            {ts}
          </span>
        </div>
      </div>
      {own && avatar}
    </div>
  );
}

/**
 * An image attachment, shown inline.
 *
 * The bytes come from an authenticated route, so `<img src="/api/...">` cannot
 * work — the global fetch interceptor only sees `fetch`. The object URL is
 * handed to the element instead, which also keeps the bearer out of any URL a
 * log could keep.
 */
function ImageAttachment({
  userId,
  chatId,
  attachment,
  downloadLabel,
}: {
  userId: string;
  chatId: string;
  attachment: ChatAttachment;
  downloadLabel: string;
}) {
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let live = true;
    attachmentObjectUrl(userId, chatId, attachment.id).then(
      (u) => live && setUrl(u),
      () => live && setFailed(true),
    );
    return () => {
      live = false;
    };
  }, [userId, chatId, attachment.id]);

  // A broken image still has a name and a size worth showing, and the
  // download route may well work — degrade to the file chip rather than
  // leaving a hole in the thread.
  if (failed) {
    return (
      <FileAttachment
        userId={userId}
        chatId={chatId}
        attachment={attachment}
      />
    );
  }
  return (
    <button
      type="button"
      onClick={() => void downloadChatAttachment(userId, chatId, attachment)}
      aria-label={`${downloadLabel}: ${attachment.filename}`}
      title={`${downloadLabel}: ${attachment.filename}`}
      className="block cursor-pointer"
    >
      {url ? (
        <img
          src={url}
          alt={attachment.filename}
          className="max-h-40 max-w-full rounded object-contain"
        />
      ) : (
        <span className="block h-20 w-32 animate-pulse rounded bg-black/10" />
      )}
    </button>
  );
}

/** A non-image attachment: name, size, download. */
function FileAttachment({
  userId,
  chatId,
  attachment,
}: {
  userId: string;
  chatId: string;
  attachment: ChatAttachment;
}) {
  const { t } = useTranslation();
  return (
    <button
      type="button"
      onClick={() => void downloadChatAttachment(userId, chatId, attachment)}
      aria-label={`${t("messages.download")}: ${attachment.filename}`}
      title={t("messages.download")}
      className="flex max-w-64 items-center gap-1.5 rounded bg-black/10 px-1.5 py-1 text-left hover:bg-black/20"
    >
      <FileText className="h-3.5 w-3.5 shrink-0" />
      <span className="min-w-0 flex-1">
        <span className="block truncate text-[11px]">{attachment.filename}</span>
        <span className="block text-[9px] opacity-70">
          {formatAttachmentSize(attachment.size)}
        </span>
      </span>
      <Download className="h-3 w-3 shrink-0 opacity-70" />
    </button>
  );
}