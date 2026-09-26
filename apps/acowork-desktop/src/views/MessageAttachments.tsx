/**
 * Shared message-row primitives for the user↔user inbox thread.
 *
 * Extracted from the original (now-deleted) MessagesView so the inbox
 * middle panel ([InboxPanel](./InboxPanel.tsx)) can render the same bubble /
 * attachment visuals without duplicating ~150 lines.
 */

import { useEffect, useState } from "react";
import { Download, FileText, Loader2 } from "lucide-react";

import { useTranslation } from "../i18n/useTranslation";
import {
  attachmentObjectUrl,
  canPreviewAttachment,
  downloadChatAttachment,
  fetchAttachmentText,
  formatAttachmentSize,
} from "../lib/user-chat-api";
import { languageForPath } from "../lib/monacoLanguage";
import { log } from "../lib/logger";
import { formatBubbleTime } from "../lib/formatTime";
import { cn } from "../lib/utils";
import type { ChatAttachment, UserChatMessage } from "../lib/types";
import { useFileEditorStore } from "../stores/fileEditorStore";
import { showToast } from "../components/common/ToastProvider";
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
  // One intentional exception: a user↔user attachment chip gains its own
  // preview hit area (see `FileAttachment`), which the agent-side chip has
  // no use for — agent attachments are workspace files that open in the
  // file tab through the attached-context list already. The chip's own
  // visual tokens still match; only the shell element differs, because a
  // nested `<button>` is invalid.
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
      onClick={() => {
        // Same download path as FileAttachment. Images show no busy
        // chip — they are usually small and the save dialog is the
        // only feedback anyway. `.catch()` keeps an unhandled
        // rejection from escaping into React.
        downloadChatAttachment(userId, chatId, attachment).catch(() => {});
      }}
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

/**
 * A non-image attachment: name, size, and two separate affordances — click
 * the body to preview a text-readable file in the right-hand Monaco tab,
 * click the bolt to save it to disk.
 *
 * The split is why the shell is a `<div>`: a `<button>` cannot contain
 * another `<button>`, and nesting them is invalid HTML that breaks click
 * routing. Non-previewable files (binaries, oversized text) keep a single
 * download hit area over the whole chip, as before.
 */
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
  const openAttachmentPreview = useFileEditorStore((s) => s.openAttachmentPreview);
  // Bytes already written to disk, `null` while idle. Rust streams the
  // transfer and pushes the count over an IPC channel, so this is real
  // progress and not a spinner guessing how long the save dialog will
  // stay open.
  const [received, setReceived] = useState<number | null>(null);
  // The body read for the preview tab is a second, independent spinner —
  // it never touches the download's byte counter.
  const [previewing, setPreviewing] = useState(false);
  const previewable = canPreviewAttachment(attachment);
  const total = attachment.size;
  // `null` when the size is unknown — then there is nothing to divide by
  // and the chip falls back to a spinner.
  const percent =
    received !== null && total > 0
      ? Math.min(100, Math.floor((received / total) * 100))
      : null;

  const handleDownload = () => {
    setReceived(0);
    downloadChatAttachment(userId, chatId, attachment, setReceived)
      .catch(() => {})
      .finally(() => setReceived(null));
  };

  const handlePreview = () => {
    setPreviewing(true);
    fetchAttachmentText(userId, chatId, attachment)
      .then((content) =>
        openAttachmentPreview({
          userId,
          chatId,
          attachmentId: attachment.id,
          fileName: attachment.filename,
          content,
          language: languageForPath(attachment.filename),
        }),
      )
      .catch((err) => {
        // Never fail silently: the click did nothing visible, so without a
        // toast the user cannot tell a 404 from a mis-click.
        log.error("[MessageAttachments] attachment preview failed:", err);
        showToast({
          type: "error",
          message: t("messages.previewFailed", { name: attachment.filename }),
        });
      })
      .finally(() => setPreviewing(false));
  };

  const busy = received !== null || previewing;

  return (
    <div className="relative max-w-64">
      <button
        type="button"
        onClick={previewable ? handlePreview : handleDownload}
        aria-label={
          previewable
            ? `${t("messages.preview")}: ${attachment.filename}`
            : `${t("messages.download")}: ${attachment.filename}`
        }
        title={previewable ? t("messages.preview") : t("messages.download")}
        disabled={previewable ? previewing : received !== null}
        className={cn(
          "flex w-full flex-col gap-0.5 rounded bg-black/10 px-1.5 py-1 text-left hover:bg-black/20 disabled:cursor-progress disabled:opacity-70",
          previewable && "pr-6",
        )}
      >
        <span className="flex items-center gap-1.5">
          <FileText className="h-3.5 w-3.5 shrink-0" />
          <span className="min-w-0 flex-1 truncate text-[11px]">{attachment.filename}</span>
          {/* Non-previewable chips have no bolt, so the trailing glyph
              stays the download affordance it always was. */}
          {!previewable &&
            (received !== null
              ? <Loader2 className="h-3 w-3 shrink-0 animate-spin opacity-70" />
              : <Download className="h-3 w-3 shrink-0 opacity-70" />)}
        </span>
        <span className="block text-[9px] opacity-70">
          {received === null
            ? formatAttachmentSize(total)
            : percent === null
              ? t("messages.saving")
              : `${formatAttachmentSize(received)} / ${formatAttachmentSize(total)} (${percent}%)`}
        </span>
        {percent !== null && (
          <span
            className="mt-0.5 block h-1 overflow-hidden rounded-full bg-black/15"
            role="progressbar"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={percent}
          >
            <span
              className="block h-full bg-current opacity-70 transition-[width] duration-200"
              style={{ width: `${percent}%` }}
            />
          </span>
        )}
      </button>
      {previewable && (
        <button
          type="button"
          onClick={handleDownload}
          aria-label={`${t("messages.download")}: ${attachment.filename}`}
          title={t("messages.download")}
          disabled={received !== null}
          className="absolute right-1 top-1 rounded p-0.5 text-text-tertiary transition-colors hover:bg-black/10 hover:text-text-secondary disabled:cursor-progress disabled:opacity-50"
        >
          {/* Also the spinner slot for the preview read: the body only opens
              the tab once it has the content, so without this the chip looks
              inert for the duration. */}
          {busy
            ? <Loader2 className="h-3 w-3 animate-spin" />
            : <Download className="h-3 w-3" />}
        </button>
      )}
    </div>
  );
}