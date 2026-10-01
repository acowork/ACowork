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
  attachmentThumbObjectUrl,
  attachmentThumbText,
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
  // Two intentional exceptions: a user↔user attachment row sits ABOVE the
  // bubble rather than inside it (the agent side already does this — see
  // the `message-attachments` block below), and a chip gains its own
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
        {/* Attachments — their own row ABOVE the bubble, exactly like the
            agent side renders chips
            ([UserWithAttachmentsBubble.tsx:142](../../components/chat/UserWithAttachmentsBubble.tsx#L142)).
            They used to live *inside* the rounded bubble next to the body,
            which is what every IM since WeChat stopped doing: the bubble
            ended up sized by its attachments, an image inherited the
            bubble's text padding, and a file with no caption rendered an
            empty rounded shell around one chip. `messages.jsonl` always
            carried them as a sibling of `body` — this is purely where the
            DOM puts them. */}
        {attachments.length > 0 && (
          <div
            data-testid="message-attachments"
            className={cn(
              "mt-2 flex max-w-[var(--content-max-width)] flex-col gap-1.5",
              own ? "items-end" : "items-start",
            )}
          >
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

        {/* Bubble wrapper — mirrors `group mt-[6px] max-w-[85%] flex flex-col
            items-start` from agent user branch. Keep `flex flex-col items-start`
            verbatim — the timestamp sibling needs that column to align its
            left edge to the bubble's left edge. */}
        <div className="group mt-[6px] max-w-[var(--content-max-width)] flex flex-col items-start">
          {/* A message with attachments and no body has no bubble at all —
              the row is the attachment. The old unconditional wrapper
              rendered an empty `max-h-48` shell in that case. */}
          {hasBody && (
            <div
              data-testid="message-bubble"
              className={cn(
                "w-full rounded-md max-h-48 overflow-y-auto",
                bubbleClass,
              )}
              style={{ fontSize: "var(--ui-font-size, 0.875rem)" }}
            >
              <div className="px-4 py-2.5 whitespace-pre-wrap break-words">
                {message.body}
              </div>
            </div>
          )}
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
 * An image attachment, shown as a thumbnail tile.
 *
 * The tile is the **generated** sidecar (`?thumb=1`, a 320 px JPEG made at
 * upload), not the original: rendering the row used to pull the whole blob,
 * so a 25 MB screenshot cost 25 MB to fill a 160 px square. The original is
 * still what the click downloads.
 *
 * Falls back to the original blob when no sidecar exists (an upload from
 * before thumbnails, or a format the decoder rejected) — the row must never
 * be a hole just because a nicety is missing.
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
  const [thumbUrl, setThumbUrl] = useState<string | null>(null);
  const [originalUrl, setOriginalUrl] = useState<string | null>(null);
  // Only set once the sidecar route has answered *and* declined, so the
  // original is fetched only for attachments that genuinely lack one.
  const [needOriginal, setNeedOriginal] = useState(false);
  // The sidecar route has answered — either with bytes or with a refusal.
  // Without this the "no url" fallback below cannot tell *not yet* from
  // *never*, and fires on the very first render, so every image falls
  // through to `FileAttachment` for the whole life of the message and the
  // thumbnail is fetched, resolved, and then thrown away.
  const [sidecarDone, setSidecarDone] = useState(false);

  useEffect(() => {
    let live = true;
    attachmentThumbObjectUrl(userId, chatId, attachment).then((u) => {
      if (!live) return;
      if (u) setThumbUrl(u);
      else setNeedOriginal(true);
      setSidecarDone(true);
    });
    return () => {
      live = false;
    };
  }, [userId, chatId, attachment]);

  useEffect(() => {
    if (!needOriginal) return;
    let live = true;
    attachmentObjectUrl(userId, chatId, attachment.id)
      .then((u) => {
        if (live) setOriginalUrl(u);
      }, () => {
        // A dead original is cosmetic — the chip below still names the
        // file and the download route may well work.
        if (live) setSidecarDone(true);
      });
    return () => {
      live = false;
    };
  }, [userId, chatId, attachment.id, needOriginal]);

  const url = thumbUrl ?? originalUrl;

  // Neither the sidecar nor the original resolved — a broken image still has
  // a name and a size worth showing, and the download route may well work,
  // so degrade to the file chip rather than leaving a hole in the thread.
  // Gated on `sidecarDone` so it fires on *refusal*, not on the first
  // render, which is what made the image tile unreachable (the thumbnail
  // resolved into a component that had already committed to the chip).
  if (sidecarDone && !url) {
    return (
      <FileAttachment
        userId={userId}
        chatId={chatId}
        attachment={attachment}
      />
    );
  }
  // A fixed tile rather than the image at natural size: a 4K screenshot
  // used to stretch the row to its full width and push every message
  // below it off screen. `object-cover` fills the square so the row
  // keeps a predictable rhythm; the alt text and the title still name
  // the file, and the whole tile is the download hit area.
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
      className="block size-40 cursor-pointer overflow-hidden rounded-md bg-black/10"
    >
      {url ? (
        <img
          src={url}
          alt={attachment.filename}
          className="size-full object-cover"
        />
      ) : (
        <span className="block size-full animate-pulse bg-black/10" />
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
 *
 * A text file small enough for the thumbnail budget also gets its opening
 * lines rendered above the chip, so the row says what the file is before
 * anyone clicks it.
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
  // The `txt` sidecar the service captured at upload — a few hundred bytes,
  // where the old on-mount read cost the entire file.
  const [thumbText, setThumbText] = useState<string | null>(null);
  const total = attachment.size;
  // `null` when the size is unknown — then there is nothing to divide by
  // and the chip falls back to a spinner.
  const percent =
    received !== null && total > 0
      ? Math.min(100, Math.floor((received / total) * 100))
      : null;

  // A missing or unreadable sidecar is a cosmetic gap, not an error: the
  // filename and size already say what this row is, and the click-to-preview
  // path below still fetches the real file and reports its own failures.
  useEffect(() => {
    let live = true;
    attachmentThumbText(userId, chatId, attachment)
      .then((t) => {
        if (live) setThumbText(t);
      })
      .catch(() => {
        if (live) setThumbText(null);
      });
    return () => {
      live = false;
    };
  }, [userId, chatId, attachment]);

  const handleDownload = () => {
    setReceived(0);
    downloadChatAttachment(userId, chatId, attachment, setReceived)
      .catch(() => {})
      .finally(() => setReceived(null));
  };

  const handlePreview = () => {
    setPreviewing(true);
    // The full body, always — the sidecar is only the first few lines and
    // handing those to Monaco would silently truncate the file in the tab.
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
    <div className="max-w-64">
      {thumbText !== null && (
        <div
          data-testid="attachment-thumbnail"
          className="mb-1 overflow-hidden rounded bg-black/10 px-1.5 py-1"
        >
          {/* A shape hint, not something to read in full — the chip click
              opens the real thing in Monaco. `text-[10px]` in a monospace
              stack; the service already capped it at 8 non-blank lines. */}
          <pre className="line-clamp-8 max-h-24 overflow-hidden whitespace-pre-wrap break-words font-mono text-[10px] leading-[1.35] opacity-80">
            {thumbText}
          </pre>
        </div>
      )}
      {/* The bolt is absolutely positioned against THIS box so it stays
          anchored to the chip, not to the thumbnail above it. */}
      <div className="relative">
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
    </div>
  );
}