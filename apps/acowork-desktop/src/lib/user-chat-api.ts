/**
 * User↔user chat HTTP client (ADR-076 §决策 8).
 *
 * Plain `fetch`, **no token plumbing**: these paths are not under
 * `/api/auth/*`, so the app-wide interceptor ([authFetch.ts](authFetch.ts))
 * attaches the bearer and handles the 401 → refresh → replay for us.
 * Threading tokens through here would be a second, weaker copy of that.
 *
 * Every route below exists only under `AUTH_MODE=multi_user`
 * (ADR-076 §决策 12); under `local` the view that uses them is never
 * mounted.
 */

import { readError } from "./auth-api";
import { getGatewayUrl } from "./config";
import type {
  ChatAttachment,
  UserChatMessage,
  UserChatMessagesPage,
  UserChatSummary,
  UserDirectoryEntry,
} from "./types";

/** Mirrors the Gateway's `DEFAULT_PAGE`. */
export const CHAT_PAGE_SIZE = 50;

function base(userId: string, chatId?: string): string {
  const root = `${getGatewayUrl()}/api/users/${encodeURIComponent(userId)}/chats`;
  return chatId ? `${root}/${encodeURIComponent(chatId)}` : root;
}

async function decode<T>(resp: Response): Promise<T> {
  if (!resp.ok) throw new Error(await readError(resp));
  return (await resp.json()) as T;
}

/** Conversations for `userId`, most recently active first. */
export async function listChats(userId: string): Promise<UserChatSummary[]> {
  const data = await decode<{ chats: UserChatSummary[] }>(
    await fetch(base(userId)),
  );
  return data.chats;
}

/**
 * One page of history, returned oldest-first within the page. `offset`
 * counts **back** from the newest message (0 = latest page).
 */
export async function listMessages(
  userId: string,
  chatId: string,
  offset = 0,
  limit = CHAT_PAGE_SIZE,
): Promise<UserChatMessagesPage> {
  const query = `?offset=${offset}&limit=${limit}`;
  return decode<UserChatMessagesPage>(
    await fetch(`${base(userId, chatId)}/messages${query}`),
  );
}

/** Send as `userId`. The Gateway takes `from` from the token, not from us. */
export async function sendChatMessage(
  userId: string,
  chatId: string,
  body: string,
  attachmentIds: string[] = [],
): Promise<UserChatMessage> {
  return decode<UserChatMessage>(
    await fetch(`${base(userId, chatId)}/messages`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ body, attachments: attachmentIds }),
    }),
  );
}

/**
 * Upload one file and get back the id the message will reference.
 *
 * `FormData` sets its own multipart boundary, so no `Content-Type` header
 * here — setting one by hand is what breaks uploads.
 */
export async function uploadChatAttachment(
  userId: string,
  chatId: string,
  file: File,
): Promise<ChatAttachment> {
  const form = new FormData();
  form.append("file", file, file.name);
  return decode<ChatAttachment>(
    await fetch(`${base(userId, chatId)}/files`, { method: "POST", body: form }),
  );
}

/**
 * Blob URLs for attachments, keyed by id.
 *
 * Downloads need the bearer token, so `<img src="/api/...">` cannot work —
 * the global interceptor only sees `fetch`. Fetch the bytes and hand the
 * element an object URL instead; that also keeps the token out of any URL
 * that could be logged or shared.
 *
 * ponytail: entries are never revoked. A desktop session sees a bounded
 * number of attachments, and each blob dies with the page anyway; add an
 * eviction pass if a long-lived session ever accumulates them.
 */
const blobUrls = new Map<string, Promise<string>>();

/** In-flight uploads are already deduped by id, so one URL per attachment. */
export function attachmentObjectUrl(
  userId: string,
  chatId: string,
  attachmentId: string,
): Promise<string> {
  let url = blobUrls.get(attachmentId);
  if (!url) {
    url = (async () => {
      const resp = await fetch(
        `${base(userId, chatId)}/files/${encodeURIComponent(attachmentId)}`,
      );
      if (!resp.ok) throw new Error(await readError(resp));
      return URL.createObjectURL(await resp.blob());
    })();
    // A failed fetch must not be cached as a permanent failure.
    url.catch(() => blobUrls.delete(attachmentId));
    blobUrls.set(attachmentId, url);
  }
  return url;
}

/** Save an attachment to disk through the browser's download path. */
export async function downloadChatAttachment(
  userId: string,
  chatId: string,
  attachment: ChatAttachment,
): Promise<void> {
  const url = await attachmentObjectUrl(userId, chatId, attachment.id);
  const a = document.createElement("a");
  a.href = url;
  a.download = attachment.filename;
  a.click();
}

/** Human-readable size, matching how the file pickers show one. */
export function formatAttachmentSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

/** Clear this user's unread counter for the conversation. */
export async function markChatRead(
  userId: string,
  chatId: string,
): Promise<void> {
  const resp = await fetch(`${base(userId, chatId)}/read`, { method: "POST" });
  if (!resp.ok) throw new Error(await readError(resp));
}

/**
 * Contacts for the "new conversation" picker: every enabled account except
 * the caller.
 *
 * The route lives in `account_api` but exists for this view — it is the one
 * account listing a non-admin may read (ADR-076 §决策 8), because a chat
 * feature with no way to name a recipient is unusable for everyone who is
 * not an admin. The Gateway owns what is exposed; this is a plain read.
 */
export async function listUserDirectory(): Promise<UserDirectoryEntry[]> {
  const data = await decode<{ users: UserDirectoryEntry[] }>(
    await fetch(`${getGatewayUrl()}/api/users/directory`),
  );
  return data.users;
}

/** `display_name` if set, else the login handle — never blank. */
export function contactLabel(entry: UserDirectoryEntry): string {
  return entry.display_name || entry.username;
}
