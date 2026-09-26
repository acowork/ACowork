/**
 * Self-check for `MessageRow` avatar wiring (ADR-076 §决策 8).
 *
 * Own row → `<UserAvatar>` with self avatar props. Peer row → the same
 * component with peer avatar props. The component is just dumb JSX,
 * so two renders around a mock avatar is enough to lock the contract
 * — `UserAvatar` itself is tested in its own file.
 */
import { describe, it, expect, vi } from "vitest";
import { render } from "@testing-library/react";
import { MessageRow } from "./MessageAttachments";
import type { UserChatMessage } from "../lib/types";

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