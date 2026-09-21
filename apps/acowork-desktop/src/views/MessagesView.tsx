/**
 * MessagesView — the user↔user inbox (ADR-076 §决策 8).
 *
 * Left: conversations. Right: the open thread + composer. Polls while
 * mounted — user chat has no push channel, the whole feature lives on the
 * Gateway's HTTP surface (§决策 8).
 *
 * Starting a conversation needs a peer id, and `GET /api/users` is
 * admin-only, so anyone but an admin would have been unable to open one.
 * The picker therefore reads `GET /api/users/directory` — the one account
 * listing every authenticated account may read (§决策 8, P7). The admin
 * sidebar `UserList` also offers "message this user", but it is a shortcut,
 * not the only door.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { Download, FileText, Paperclip, Send, X } from "lucide-react";

import { useAuthStore } from "../stores/authStore";
import { chatIdOf, useUserChatStore } from "../stores/userChatStore";
import { useTranslation } from "../i18n/useTranslation";
import { Dropdown } from "../components/common/Dropdown";
import {
  attachmentObjectUrl,
  contactLabel,
  downloadChatAttachment,
  formatAttachmentSize,
  listUserDirectory,
} from "../lib/user-chat-api";
import { formatBubbleTime } from "../lib/formatTime";
import { inputBase } from "../lib/ui-styles";
import { cn } from "../lib/utils";
import type {
  ChatAttachment,
  UserChatMessage,
  UserChatSummary,
  UserDirectoryEntry,
} from "../lib/types";

export function MessagesView() {
  const { t } = useTranslation();

  const mode = useAuthStore((s) => s.mode);
  const status = useAuthStore((s) => s.status);
  const self = useAuthStore((s) => s.account);

  const chats = useUserChatStore((s) => s.chats);
  const activePeerId = useUserChatStore((s) => s.activePeerId);
  const activePeerLabel = useUserChatStore((s) => s.activePeerLabel);
  const messages = useUserChatStore((s) => s.messages);
  const loadingMessages = useUserChatStore((s) => s.loadingMessages);
  const sending = useUserChatStore((s) => s.sending);
  const error = useUserChatStore((s) => s.error);
  const pendingAttachments = useUserChatStore((s) => s.pendingAttachments);
  const uploading = useUserChatStore((s) => s.uploading);
  const removePendingAttachment = useUserChatStore(
    (s) => s.removePendingAttachment,
  );

  const [draft, setDraft] = useState("");
  const [contacts, setContacts] = useState<UserDirectoryEntry[]>([]);
  const fileInput = useRef<HTMLInputElement>(null);
  const bottomRef = useRef<HTMLDivElement>(null);

  const ready = mode === "multi_user" && status === "logged_in" && self != null;

  useEffect(() => {
    if (!ready) return;
    const store = useUserChatStore.getState();
    void store.refreshChats();
    // Poll while this view is open. The loop is ref-counted and shared with
    // the nav-bar unread badge, so the two never double-poll.
    return store.startPolling();
  }, [ready]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ block: "end" });
  }, [messages.length, activePeerId]);

  // Contacts for the "new conversation" picker. Best-effort: an empty picker
  // is a degraded inbox, not a broken one — replies still work.
  useEffect(() => {
    if (!ready) return;
    let live = true;
    listUserDirectory()
      .then((users) => {
        if (live) setContacts(users);
      })
      .catch(() => {
        if (live) setContacts([]);
      });
    return () => {
      live = false;
    };
  }, [ready]);

  const open = useCallback((peerId: string) => {
    setDraft("");
    void useUserChatStore.getState().openChat(peerId);
  }, []);

  const submit = useCallback(async () => {
    const body = draft.trim();
    const pending = useUserChatStore.getState().pendingAttachments;
    if (!body && pending.length === 0) return;
    setDraft("");
    // Attachments are consumed by `send`, so a failure must restore the text
    // *and* leave the files queued — hence the id list is passed explicitly.
    const ok = await useUserChatStore
      .getState()
      .send(body, pending.map((a) => a.id));
    if (!ok) setDraft(body); // keep the text so it is not lost
  }, [draft]);

  const pickFiles = useCallback((files: FileList | null) => {
    if (!files) return;
    const store = useUserChatStore.getState();
    for (const file of Array.from(files)) void store.attach(file);
  }, []);

  const activeLabel = activePeerId
    ? (chats.find((c) => c.peer_user_id === activePeerId)?.peer_display_name ??
      activePeerLabel ??
      activePeerId)
    : null;

  return (
    <div className="flex h-full w-full overflow-hidden rounded-xl bg-page-bg">
      {/* Conversations */}
      <aside className="flex w-64 shrink-0 flex-col overflow-hidden border-r border-nav-divider/40 dark:border-zinc-700/40">
        <div className="flex h-9 shrink-0 items-center gap-2 px-3 text-xs font-semibold text-text-secondary">
          <span className="truncate">{t("messages.title")}</span>
          {/* No contacts (endpoint down, or nobody else exists yet) ⇒ no
              picker. A degraded inbox still replies fine. */}
          {contacts.length > 0 && (
            <Dropdown
              size="small"
              className="ml-auto w-28 shrink-0 font-normal"
              value=""
              onChange={(peerId) => {
                if (peerId) open(peerId);
              }}
              options={contacts.map((c) => ({
                value: c.user_id,
                label: contactLabel(c),
              }))}
              placeholder={{ value: "", label: t("messages.newChat") }}
              aria-label={t("messages.newChat")}
            />
          )}
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto">
          {chats.length === 0 ? (
            <p className="px-3 py-2 text-[11px] text-text-tertiary">
              {t("messages.empty")}
            </p>
          ) : (
            chats.map((chat) => (
              <ConversationRow
                key={chat.chat_id}
                chat={chat}
                active={chat.peer_user_id === activePeerId}
                onSelect={() => open(chat.peer_user_id)}
              />
            ))
          )}
        </div>
      </aside>

      {/* Thread */}
      <section className="flex min-w-0 flex-1 flex-col overflow-hidden">
        {activePeerId == null ? (
          <div className="flex flex-1 items-center justify-center p-8 text-center text-xs text-text-tertiary">
            {t("messages.pick")}
          </div>
        ) : (
          <>
            <header className="flex h-9 shrink-0 items-center px-4 text-xs font-semibold text-text-secondary">
              <span className="truncate">{activeLabel}</span>
            </header>

            {error && (
              <div role="alert" className="shrink-0 px-4 py-1 text-[11px] text-red-500">
                {t("messages.loadFailed")}
              </div>
            )}

            <div className="min-h-0 flex-1 overflow-y-auto px-4 py-2">
              {messages.length === 0 && !loadingMessages && (
                <p className="py-2 text-[11px] text-text-tertiary">
                  {t("messages.noMessages")}
                </p>
              )}
              {messages.map((message, i) => (
                <MessageRow
                  key={`${message.ts}-${i}`}
                  message={message}
                  own={message.from === self?.user_id}
                  youLabel={t("messages.you")}
                  userId={self?.user_id ?? ""}
                  chatId={
                    self ? chatIdOf(activePeerId ?? "", self.user_id) : ""
                  }
                />
              ))}
              <div ref={bottomRef} />
            </div>

            <div className="shrink-0 border-t border-nav-divider/40 p-3 dark:border-zinc-700/40">
              {(pendingAttachments.length > 0 || uploading.length > 0) && (
                <div className="mb-2 flex flex-wrap gap-1">
                  {pendingAttachments.map((a) => (
                    <span
                      key={a.id}
                      className="flex items-center gap-1 rounded border border-border-outer bg-modal-surface px-1.5 py-0.5 text-[10px] text-text-secondary"
                    >
                      <span className="max-w-40 truncate">{a.filename}</span>
                      <span className="text-text-tertiary">
                        {formatAttachmentSize(a.size)}
                      </span>
                      <button
                        type="button"
                        onClick={() => removePendingAttachment(a.id)}
                        aria-label={`${t("messages.removeAttachment")}: ${a.filename}`}
                        title={t("messages.removeAttachment")}
                        className="text-text-tertiary hover:text-red-500"
                      >
                        <X className="h-3 w-3" />
                      </button>
                    </span>
                  ))}
                  {uploading.map((name) => (
                    <span
                      key={name}
                      className="rounded border border-dashed border-border-outer px-1.5 py-0.5 text-[10px] text-text-tertiary"
                    >
                      {t("messages.uploading")}…
                    </span>
                  ))}
                </div>
              )}
              <div className="flex items-end gap-2">
                <input
                  ref={fileInput}
                  type="file"
                  multiple
                  className="hidden"
                  onChange={(e) => {
                    pickFiles(e.target.files);
                    // Reset so picking the same file twice fires again.
                    e.target.value = "";
                  }}
                />
                <button
                  type="button"
                  onClick={() => fileInput.current?.click()}
                  aria-label={t("messages.attach")}
                  title={t("messages.attach")}
                  className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-text-secondary hover:bg-nav-item-hover"
                >
                  <Paperclip className="h-4 w-4" />
                </button>
                <textarea
                  value={draft}
                  onChange={(e) => setDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey) {
                      e.preventDefault();
                      void submit();
                    }
                  }}
                  rows={2}
                  placeholder={t("messages.placeholder")}
                  aria-label={t("messages.placeholder")}
                  className={cn(inputBase, "resize-none text-xs")}
                />
                <button
                  type="button"
                  onClick={() => void submit()}
                  disabled={
                    sending ||
                    (draft.trim().length === 0 && pendingAttachments.length === 0)
                  }
                  aria-label={t("messages.send")}
                  title={t("messages.send")}
                  className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-white disabled:opacity-40"
                  style={{ backgroundColor: "var(--color-accent)" }}
                >
                  <Send className="h-4 w-4" />
                </button>
              </div>
            </div>
          </>
        )}
      </section>
    </div>
  );
}

function ConversationRow({
  chat,
  active,
  onSelect,
}: {
  chat: UserChatSummary;
  active: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onSelect}
      className={cn(
        "flex w-full flex-col gap-0.5 px-3 py-1.5 text-left hover:bg-nav-item-hover",
        active && "bg-nav-item-hover",
      )}
    >
      <span className="flex items-center gap-1">
        <span className="truncate text-xs text-text-secondary">
          {chat.peer_display_name}
        </span>
        {chat.unread_count > 0 && (
          <span
            className="ml-auto shrink-0 rounded-full px-1.5 text-[10px] font-medium text-white"
            style={{ backgroundColor: "var(--color-accent)" }}
          >
            {chat.unread_count}
          </span>
        )}
      </span>
      <span className="truncate text-[10px] text-text-tertiary">
        {chat.last_message_preview}
      </span>
    </button>
  );
}

function MessageRow({
  message,
  own,
  youLabel,
  userId,
  chatId,
}: {
  message: UserChatMessage;
  own: boolean;
  youLabel: string;
  /** Signed-in account, for building attachment URLs (token goes via fetch). */
  userId: string;
  chatId: string;
}) {
  const { t } = useTranslation();
  const attachments = message.attachments ?? [];
  const hasBody = message.body.trim().length > 0;

  return (
    <div className={cn("mb-2 flex", own ? "justify-end" : "justify-start")}>
      <div
        className={cn(
          "max-w-[70%] rounded-lg px-2.5 py-1.5 text-xs",
          own ? "text-white" : "bg-zinc-100 text-text-secondary dark:bg-zinc-800",
        )}
        style={own ? { backgroundColor: "var(--color-accent)" } : undefined}
      >
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
        {hasBody && (
          <div className="whitespace-pre-wrap break-words">{message.body}</div>
        )}
        <div
          className={cn("mt-0.5 text-[9px]", own ? "text-white/70" : "text-text-tertiary")}
        >
          {own ? youLabel : ""} {formatBubbleTime(message.ts * 1000)}
        </div>
      </div>
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
