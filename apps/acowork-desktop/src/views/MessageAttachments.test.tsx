/**
 * Self-check for `MessageRow` avatar wiring (ADR-076 §决策 8) and for the
 * attachment row staying OUTSIDE the bubble.
 *
 * Own row → `<UserAvatar>` with self avatar props. Peer row → the same
 * component with peer avatar props. The component is just dumb JSX,
 * so two renders around a mock avatar is enough to lock the contract
 * — `UserAvatar` itself is tested in its own file.
 *
 * The attachment assertions guard the IM convention (a file or image is
 * its own message, not decoration inside a text bubble): `messages.jsonl`
 * stores attachments as a sibling of `body`, and the renderer used to
 * nest them inside the rounded bubble, so a caption-less file rendered an
 * empty bubble shell and an image inherited the bubble's text padding.
 */
import { describe, it, expect, vi } from "vitest";
import { render } from "@testing-library/react";
import { MessageRow } from "./MessageAttachments";
import type { ChatAttachment, UserChatMessage } from "../lib/types";

vi.mock("../components/common/UserAvatar", () => ({
  UserAvatar: (props: Record<string, unknown>) => (
    <div
      data-testid="user-avatar"
      data-display-name={props.displayName ?? ""}
      data-avatar-url={props.avatarUrl ?? ""}
      data-builtin-id={props.builtinAvatarId ?? ""}
    />
  ),
}));

const MSG: UserChatMessage = {
  ts: 1,
  from: "u-self",
  kind: "text",
  body: "hi",
};

function row(message: UserChatMessage) {
  return (
    <MessageRow
      message={message}
      own
      youLabel="You"
      userId="u-self"
      chatId="c"
      ownAvatarUrl="assets/me.png"
      ownBuiltinAvatarId="icon-01"
      ownDisplayName="Me"
      peerAvatarUrl="assets/peer.png"
      peerBuiltinAvatarId="icon-02"
      peerDisplayName="Peer"
    />
  );
}

describe("attachment row placement", () => {
  it("renders attachments beside the bubble, never inside it", () => {
    const attachment: ChatAttachment = {
      id: "a1",
      filename: "notes.txt",
      mime: "text/plain",
      size: 12,
    };
    const { queryByTestId } = render(
      row({ ...MSG, attachments: [attachment] }),
    );
    const bubble = queryByTestId("message-bubble");
    const attachments = queryByTestId("message-attachments");
    expect(bubble).not.toBeNull();
    expect(attachments).not.toBeNull();
    // The bug this locks: the chip was a descendant of the rounded
    // bubble, which is what made a caption-less file render an empty
    // bubble around itself.
    expect(attachments!.contains(bubble!)).toBe(false);
    expect(bubble!.contains(attachments!)).toBe(false);
  });

  it("renders no bubble for an attachment-only message", () => {
    const attachment: ChatAttachment = {
      id: "a1",
      filename: "notes.txt",
      mime: "text/plain",
      size: 12,
    };
    const { queryByTestId } = render(
      row({ ...MSG, body: "", attachments: [attachment] }),
    );
    expect(queryByTestId("message-bubble")).toBeNull();
    expect(queryByTestId("message-attachments")).not.toBeNull();
  });

  it("shows the text sidecar as a thumbnail, not the whole file", async () => {
    // `thumb: "txt"` is the only signal the client gets that a preview
    // exists — the size heuristic that used to live here is gone; the
    // service decides at upload.
    const attachment: ChatAttachment = {
      id: "a1",
      filename: "README.md",
      mime: "text/markdown",
      size: 16095,
      thumb: "txt",
    };
    const seen: string[] = [];
    vi.stubGlobal("fetch", (url: string) => {
      seen.push(String(url));
      return Promise.resolve(
        new Response("# acowork\ncore gateway", { status: 200 }),
      );
    });
    try {
      const { findByTestId } = render(row({ ...MSG, attachments: [attachment] }));
      const thumb = await findByTestId("attachment-thumbnail");
      expect(thumb.textContent).toContain("# acowork");
      // The sidecar route specifically — reading the whole blob is the
      // click-to-preview path's job and must not happen on mount.
      expect(seen.some((u) => u.includes("files/a1?thumb=1"))).toBe(true);
      expect(seen.some((u) => u.includes("files/a1") && !u.includes("thumb=1"))).toBe(
        false,
      );
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("renders an image as a tile, not the file chip, once the sidecar lands", async () => {
    // The bug this locks: the "no url" fallback tested only `!url`, which
    // is also true on the very first render while the sidecar is still in
    // flight. It therefore returned `FileAttachment` immediately and for
    // good, so an image *never* became a tile — the thumbnail resolved a
    // few ms later into a component that had already committed to the
    // chip, and the row showed only a filename and a download bolt.
    const attachment: ChatAttachment = {
      id: "img1",
      filename: "shot.png",
      mime: "image/png",
      size: 2_400_000,
      thumb: "jpg",
    };
    const seen: string[] = [];
    vi.stubGlobal("fetch", (url: string) => {
      seen.push(String(url));
      return Promise.resolve(new Response(new Blob(["x"]), { status: 200 }));
    });
    // jsdom has no `URL.createObjectURL`, and the object URL is the only
    // thing the tile's `src` carries, so stub it rather than asserting on
    // a decoded image (which jsdom never does either).
    const prior = URL.createObjectURL;
    URL.createObjectURL = () => "blob:thumb";
    try {
      const { findByAltText, queryByTestId } = render(
        row({ ...MSG, attachments: [attachment] }),
      );
      // The tile only exists once the sidecar promise resolves.
      const img = await findByAltText("shot.png");
      expect(img.getAttribute("src")).toBe("blob:thumb");
      expect(queryByTestId("attachment-thumbnail")).toBeNull();
      expect(seen.some((u) => u.includes("files/img1") && u.includes("thumb=1"))).toBe(true);
    } finally {
      URL.createObjectURL = prior;
      vi.unstubAllGlobals();
    }
  });
});

describe("MessageRow avatar wiring", () => {
  it("own row renders UserAvatar with own avatar props", () => {
    const { getByTestId } = render(
      <MessageRow
        message={MSG}
        own
        youLabel="You"
        userId="u-self"
        chatId="c"
        ownAvatarUrl="assets/me.png"
        ownBuiltinAvatarId="icon-01"
        ownDisplayName="Me"
        peerAvatarUrl="assets/peer.png"
        peerBuiltinAvatarId="icon-02"
        peerDisplayName="Peer"
      />,
    );
    const avatars = getByTestId("user-avatar");
    expect(avatars.getAttribute("data-display-name")).toBe("Me");
    expect(avatars.getAttribute("data-avatar-url")).toBe("assets/me.png");
    expect(avatars.getAttribute("data-builtin-id")).toBe("icon-01");
  });

  it("peer row renders UserAvatar with peer avatar props", () => {
    const { getByTestId } = render(
      <MessageRow
        message={MSG}
        own={false}
        youLabel="You"
        userId="u-self"
        chatId="c"
        ownAvatarUrl="assets/me.png"
        ownBuiltinAvatarId="icon-01"
        ownDisplayName="Me"
        peerAvatarUrl="assets/peer.png"
        peerBuiltinAvatarId="icon-02"
        peerDisplayName="Peer"
      />,
    );
    const avatars = getByTestId("user-avatar");
    expect(avatars.getAttribute("data-display-name")).toBe("Peer");
    expect(avatars.getAttribute("data-avatar-url")).toBe("assets/peer.png");
    expect(avatars.getAttribute("data-builtin-id")).toBe("icon-02");
  });
});