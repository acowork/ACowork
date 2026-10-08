/**
 * PermissionDialog — the visibility toggle must not hand-roll its own
 * track + thumb.
 *
 * The defect this pins: the toggle used to be an inline
 * `<button role="switch">` whose thumb carried only
 * `absolute top-0.5 … translate-x-*` and NO `left-*`. With `left: auto`
 * the thumb falls back to its *static* position, which inside a
 * <button> is the inline box under Blink's UA `text-align: center`
 * (Tailwind preflight does not reset it). The static left is
 * (track 36px - thumb 16px) / 2 = 10px, so the dot rendered 12px from
 * the leading edge when off, and 10 + 16 = 26px when on — the right
 * edge then landed at 26 + 16 = 42px against a 36px track, i.e. the dot
 * escaped the switch.
 *
 * Two invariants are asserted against the RENDERED DOM (jsdom-safe,
 * no network):
 *   1. The toggle is the shared `<Switch>` — a real `role="switch"`
 *      checkbox that is keyboard-operable, carrying an accessible name.
 *   2. The thumb is explicitly anchored on the leading edge (`left-0.5`)
 *      and centered with `top-1/2 -translate-y-1/2`, so no static-position
 *      fallback can ever apply. It never uses a bare `translate-x-*`
 *      offset for the OFF state.
 */
import { describe, expect, it, vi, beforeEach } from "vitest";

// Locale-independent: t() echoes the key so assertions pin exact hooks.
vi.mock("../../i18n/useTranslation", () => ({
  useTranslation: () => ({ t: (k: string) => k }),
}));

vi.mock("../../lib/config", () => ({
  getGatewayUrl: () => "http://localhost:19876",
}));

// The perms payload is the only thing that must resolve. `fetchDirectory`
// is best-effort inside the component — a rejection just empties the
// guest picker — so it is left unmocked.
const PERMS = {
  owner_user_id: "u-owner",
  visibility: "private",
  guests: [] as string[],
  can_manage: true,
  can_attribute: true,
  // A Gateway that reports the data-state answer (ADR-087 D8). Pinned
  // explicitly so the fallback branch is not what the suite exercises.
  can_set_visibility: true,
  ownerless: false,
};

/** An ownerless payload — the state every pre-ADR-087 resource starts in. */
const OWNERLESS_PERMS = {
  ...PERMS,
  owner_user_id: null,
  ownerless: true,
  // The whole point of the field: an admin passes `can_attribute`
  // (permission) yet cannot publish (data state).
  can_set_visibility: false,
};

vi.mock("../../lib/gateway-api", () => ({
  fetchNodePermissions: vi.fn(async () => PERMS),
  fetchAgentPermissions: vi.fn(async () => PERMS),
  patchNodeVisibility: vi.fn(async () => {}),
  patchAgentVisibility: vi.fn(async () => {}),
  patchNodeGuests: vi.fn(async () => {}),
  patchAgentGuests: vi.fn(async () => {}),
  patchNodeOwner: vi.fn(async () => {}),
  patchAgentOwner: vi.fn(async () => {}),
  patchNodeName: vi.fn(async () => {}),
}));

// The name sources: the admin-only full roster and the contact-picker
// directory. Mocked (not stubbed to one fixed list) so a test can state
// what each returns — the dialog picks between them by role.
vi.mock("../../lib/auth-api", () => ({
  fetchDirectory: vi.fn(async () => [] as DirectoryRow[]),
  // Admin path: the full roster, disabled accounts included. `GET /api/users`
  // is admin-only, which is fine — only an owner/admin can edit the list
  // this dialog edits.
  fetchAccounts: vi.fn(async () => [] as AccountRow[]),
}));

type DirectoryRow = { user_id: string; username: string; display_name: string };
type AccountRow = DirectoryRow & {
  role: "user" | "admin";
  avatar?: string | null;
  builtin_avatar?: string | null;
  disabled_at?: string | null;
};

// The claim button is admin-only and reads the signed-in account from the
// store, so the default is a logged-in admin with a real id. Mutable so a
// test can act as a non-admin — the name source splits on exactly that.
const ACCOUNT = {
  user_id: "me-admin",
  username: "me",
  role: "admin" as "admin" | "user",
  display_name: "Me",
};

vi.mock("../../stores/authStore", () => ({
  useAuthStore: {
    getState: () => ({ accessToken: "t", account: ACCOUNT }),
  },
}));

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { PermissionDialog, type PermissionTarget } from "./PermissionDialog";
import * as gatewayApi from "../../lib/gateway-api";
import * as authApi from "../../lib/auth-api";

const NODE: PermissionTarget = { kind: "node", id: "n-1", name: "My Node" };

/** The owner cell — carries `data-owner`, so it stays addressable when a
 *  guest row shows the same person (the roster legitimately lists the
 *  owner too). An admin renders it as a `<select>` (assignment is an
 *  admin right, ADR-087 D7); everyone else gets a read-only `<span>`. */
function ownerCell(): HTMLElement {
  const el = document.querySelector<HTMLElement>("[data-owner]");
  if (!el) throw new Error("owner cell not found");
  return el;
}

/** The owner NAME the cell actually displays.
 *
 *  Reading `textContent` is wrong for the picker: a `<select>`'s text is
 *  every option concatenated, so it names accounts that are not the
 *  owner. The question these rows ask is "who is the owner called?",
 *  which for a select is its *selected* option. */
function ownerLabel(): string {
  const el = ownerCell();
  if (el.tagName === "SELECT") {
    const sel = el as unknown as HTMLSelectElement;
    return sel.options[sel.selectedIndex]?.textContent ?? "";
  }
  return el.textContent ?? "";
}

beforeEach(() => {
  // `clearAllMocks` only drops recorded calls — a `mockResolvedValue`
  // from a previous test would survive and leak its payload into the
  // next one. Reset, then restore the default "owned" payload, so each
  // test states its own starting row.
  vi.resetAllMocks();
  const api = gatewayApi as unknown as Record<string, ReturnType<typeof vi.fn>>;
  api.fetchNodePermissions.mockResolvedValue(PERMS);
  api.fetchAgentPermissions.mockResolvedValue(PERMS);
  api.patchNodeVisibility.mockResolvedValue(undefined);
  api.patchAgentVisibility.mockResolvedValue(undefined);
  api.patchNodeGuests.mockResolvedValue(undefined);
  api.patchAgentGuests.mockResolvedValue(undefined);
  api.patchNodeOwner.mockResolvedValue(undefined);
  api.patchAgentOwner.mockResolvedValue(undefined);
  api.patchNodeName.mockResolvedValue(undefined);
  // Same reset hazard for both name sources.
  authApi.fetchDirectory.mockResolvedValue([]);
  authApi.fetchAccounts.mockResolvedValue([]);
});

/** The toggle's visible track (the sibling of the real checkbox input). */
function trackOf(sw: HTMLElement): HTMLElement {
  const el = sw.parentElement?.querySelector<HTMLElement>('[aria-hidden="true"]');
  if (!el) throw new Error("switch track not found");
  return el;
}

/** The circular thumb — always the last aria-hidden child of the track's parent. */
function thumbOf(sw: HTMLElement): HTMLElement {
  const all = sw.parentElement?.querySelectorAll<HTMLElement>('[aria-hidden="true"]');
  const thumb = all?.[all.length - 1];
  if (!thumb) throw new Error("switch thumb not found");
  return thumb;
}

async function renderOpen() {
  render(<PermissionDialog open target={NODE} onClose={() => {}} />);
  const sw = await screen.findByRole("switch");
  return sw;
}

describe("PermissionDialog visibility toggle", () => {
  it("renders the shared Switch (a named role=switch checkbox), not a bare button", async () => {
    const sw = await renderOpen();
    expect(sw.tagName).toBe("INPUT");
    expect(sw.getAttribute("type")).toBe("checkbox");
    // The old <button role="switch"> had no accessible name at all.
    expect(sw.getAttribute("aria-label")).toBe("permissionDialog.visibility");
  });

  it("anchors the thumb on the leading edge instead of the static position", async () => {
    const sw = await renderOpen();
    const thumb = thumbOf(sw);
    const cls = thumb.className;

    // Explicit horizontal anchor — this is the load-bearing assertion.
    expect(cls).toMatch(/(^|\s)left-0\.5(\s|$)/);
    // Explicit vertical centering, not a magic top offset.
    expect(cls).toMatch(/(^|\s)top-1\/2(\s|$)/);
    expect(cls).toMatch(/(^|\s)-translate-y-1\/2(\s|$)/);
    // The OFF state must come from the anchor alone, never a translate
    // nudge — a bare translate-x-* is what pushed the dot out of the track.
    expect(cls).not.toMatch(/(^|\s)translate-x-\d/);
  });

  it("slides the thumb by its own width when checked, keeping it inside the track", async () => {
    const sw = await renderOpen();
    await waitFor(() => expect(trackOf(sw).className).toMatch(/h-5/));

    const track = trackOf(sw);
    const thumb = thumbOf(sw);
    // md preset: 36x20 track, 16x16 thumb → translate-x-full (16px) from
    // left-0.5 (2px) lands the trailing edge at 2 + 16 = 18px inside a
    // 36px track, with the same 2px gap on both sides.
    expect(track.className).toMatch(/w-9/);
    expect(thumb.className).toMatch(/h-4/);
    expect(thumb.className).toMatch(/w-4/);
    expect(thumb.className).toMatch(/peer-checked:translate-x-full/);
  });

  it("toggles visibility between private and public and marks Save dirty", async () => {
    const sw = await renderOpen();
    // Server payload is private → the switch starts OFF.
    expect((sw as HTMLInputElement).checked).toBe(false);

    fireEvent.click(sw);

    await waitFor(() => expect((sw as HTMLInputElement).checked).toBe(true));
    // The Save button only enables once the draft diverges from the server.
    const save = screen.getByRole("button", { name: "common.confirm" }) as HTMLButtonElement;
    expect(save.disabled).toBe(false);
  });

  // The reported bug: flip the switch, press Save, and the switch snaps
  // back to off. Two independent causes had to be fixed together —
  //   1. server: the PATCH answered 200 while storing the old value
  //      (ownerless ⇒ not Shared normalisation), and
  //   2. client: on a rejected write the draft stayed flipped, so a
  //      failed save looked identical to a save that never happened.
  // This pins the client half: a rejected write must revert the draft to
  // the last known server value AND surface the reason.
  it("reverts the switch to the server value and shows the error when the save is rejected", async () => {
    const { patchNodeVisibility } = await import("../../lib/gateway-api");
    (patchNodeVisibility as ReturnType<typeof vi.fn>).mockRejectedValueOnce(
      new Error(
        "Failed to update visibility: this node has no owner yet, so it cannot be made `public`",
      ),
    );

    const sw = await renderOpen();
    expect((sw as HTMLInputElement).checked).toBe(false);
    fireEvent.click(sw);
    await waitFor(() => expect((sw as HTMLInputElement).checked).toBe(true));

    const save = screen.getByRole("button", { name: "common.confirm" });
    fireEvent.click(save);

    // The draft snaps back — a rejected write must never leave the UI
    // showing a value the server does not hold.
    await waitFor(() => expect((sw as HTMLInputElement).checked).toBe(false));

    // …and the reason is visible, not swallowed.
    await waitFor(() =>
      expect(screen.getByText(/no owner yet/i)).toBeDefined(),
    );
  });

  // An ownerless resource is admin-only and unpublishable: the Gateway
  // 409s `visibility: shared` / `public` on it. Every agent and node that
  // predates ADR-087 starts ownerless, so without an in-dialog claim the
  // switch is a dead end for exactly the users most likely to try it.
  describe("visibility state label", () => {
    // The row used to render BOTH names as a static legend ("私有  共享").
    // Nothing aligned the spans to the switch's two positions, and both
    // names read as equally current while the track already shows which
    // one is. One label naming the state in effect is unambiguous.
    it("shows only the state that is in effect", async () => {
      await renderOpen();
      await screen.findByText("permissionDialog.visibilityOffHint");
      expect(screen.getByText("permissionDialog.visibilityPrivate")).toBeDefined();
      // The off state is current — the "on" name must not also be there.
      expect(screen.queryByText("permissionDialog.visibilityPublic")).toBeNull();
    });

    it("flips the label when the switch is turned on", async () => {
      const sw = await renderOpen();
      await screen.findByText("permissionDialog.visibilityOffHint");
      fireEvent.click(sw);
      await waitFor(() => expect((sw as HTMLInputElement).checked).toBe(true));
      expect(screen.getByText("permissionDialog.visibilityPublic")).toBeDefined();
      expect(screen.queryByText("permissionDialog.visibilityPrivate")).toBeNull();
    });

    it("uses the agent vocabulary for an agent target", async () => {
      render(
        <PermissionDialog
          open
          target={{ kind: "agent", id: "a-1", name: "My Agent" }}
          onClose={() => {}}
        />,
      );
      await screen.findByText("permissionDialog.visibilityOffHint");
      // A node says "public/private", an agent says "shared/private" —
      // the label must come from the target's own vocabulary.
      expect(screen.getByText("permissionDialog.visibilityPrivate")).toBeDefined();
      expect(screen.queryByText("permissionDialog.visibilityPublic")).toBeNull();
    });
  });

  describe("name resolution", () => {
    // A roster must name every id on it. `/api/users/directory` is a
    // contact picker: it drops the caller and every disabled account, so
    // it cannot answer "who is on this list" — an owner who claims a
    // resource (owner = me) and a revoked account both rendered as raw
    // UUIDs. Admins read the full roster instead, which is also the only
    // role that can edit this list.
    it("reads the full account roster for an admin", async () => {
      authApi.fetchAccounts.mockResolvedValue([
        { user_id: "u-owner", username: "bob", display_name: "Bob Bobson", role: "user" },
      ]);
      await renderOpen();
      await screen.findByText("permissionDialog.visibilityOffHint");
      // The directory is not consulted at all on the admin path.
      expect(authApi.fetchDirectory).not.toHaveBeenCalled();
      expect(ownerLabel()).toBe("Bob Bobson");
    });

    it("names a disabled guest, which the directory would have dropped", async () => {
      authApi.fetchAccounts.mockResolvedValue([
        { user_id: "u-dept", username: "dan", display_name: "Dan Departed", role: "user", disabled_at: "2026-01-01" },
      ]);
      (gatewayApi.fetchNodePermissions as ReturnType<typeof vi.fn>).mockResolvedValue({
        ...PERMS,
        guests: ["u-dept"],
      });
      await renderOpen();
      await screen.findByText("permissionDialog.visibilityOffHint");
      // A roster entry that shows a UUID is unreadable, and the departed
      // account is exactly the one an owner needs to identify to revoke.
      expect(screen.getAllByTitle("Dan Departed").length).toBeGreaterThan(0);
      expect(screen.queryByTitle("u-dept")).toBeNull();
    });

    it("names the owner when the roster has them but the directory would not", async () => {
      // Owner == the signed-in admin (`me-admin` in the store mock) — the
      // directory excludes the caller by design.
      (gatewayApi.fetchNodePermissions as ReturnType<typeof vi.fn>).mockResolvedValue({
        ...PERMS,
        owner_user_id: "me-admin",
      });
      await renderOpen();
      await screen.findByText("permissionDialog.visibilityOffHint");
      // The caller is in the pick list under their display name, not their
      // id — the roster came back empty, so only `me` can name them.
      expect(ownerLabel()).toBe("Me");
    });

    it("falls back to the directory for a non-admin viewer", async () => {
      ACCOUNT.role = "user";
      try {
        authApi.fetchDirectory.mockResolvedValue([
          { user_id: "u-owner", username: "bob", display_name: "Bob Bobson" },
        ]);
        await renderOpen();
        await screen.findByText("permissionDialog.visibilityOffHint");
        // `GET /api/users` is admin-only; a non-admin must not call it.
        expect(authApi.fetchAccounts).not.toHaveBeenCalled();
        expect(ownerCell().title).toBe("Bob Bobson");
      } finally {
        ACCOUNT.role = "admin";
      }
    });

    it("shows the raw id only when the account is genuinely unresolvable", async () => {
      // Neither the roster nor the caller's own record has it: a
      // soft-deleted account. The id must still render — showing a blank
      // would make the row look ownerless and invite a second claim.
      await renderOpen();
      await screen.findByText("permissionDialog.visibilityOffHint");
      // The id survives as its own option and stays the selected one.
      expect(ownerLabel()).toBe("u-owner");
    });

    it("names the caller in the guest roster, whom the directory drops", async () => {
      // The caller is a guest on this resource. `/api/users/directory`
      // excludes `ctx.user_id` by design, so the roster can never name
      // them — they used to render as a raw UUID in italic while every
      // other guest showed a name, which reads as "corrupt id" rather
      // than "you". `me` is the only source that can answer.
      ACCOUNT.role = "user";
      try {
        authApi.fetchDirectory.mockResolvedValue([
          { user_id: "u-owner", username: "bob", display_name: "Bob Bobson" },
        ]);
        (gatewayApi.fetchNodePermissions as ReturnType<typeof vi.fn>).mockResolvedValue({
          ...PERMS,
          guests: ["me-admin", "u-owner"],
        });
        await renderOpen();
        await screen.findByText("permissionDialog.visibilityOffHint");
        // Named, not an id — and not a second row for the same person.
        expect(screen.getAllByTitle("Me").length).toBeGreaterThan(0);
        expect(screen.queryByText("me-admin")).toBeNull();
      } finally {
        ACCOUNT.role = "admin";
      }
    });

    it("does not duplicate the caller when the roster already listed them", async () => {
      // The admin path's roster is the complete set and includes the
      // caller, so the `users` append must be a no-op rather than a
      // second row for one account — a duplicate checkbox would make
      // revoking look like it needed two clicks.
      authApi.fetchAccounts.mockResolvedValue([
        { user_id: "me-admin", username: "me", display_name: "Me", role: "admin" },
        { user_id: "u-owner", username: "bob", display_name: "Bob Bobson", role: "user" },
      ]);
      (gatewayApi.fetchNodePermissions as ReturnType<typeof vi.fn>).mockResolvedValue({
        ...PERMS,
        guests: ["me-admin"],
      });
      await renderOpen();
      await screen.findByText("permissionDialog.visibilityOffHint");
      expect(screen.getAllByTitle("Me").length).toBe(1);
    });
  });

  describe("owner assignment", () => {
    // `PATCH .../owner` is the Admin tier (ADR-087 D7), so the picker is
    // the admin's way to say who owns a resource. The whole point of the
    // change: before it existed, the only owner the UI could produce was
    // whoever was clicking, i.e. ownership was unassignable.
    function ownerSelect(): HTMLSelectElement {
      const el = screen.getByRole("combobox", {
        name: "permissionDialog.owner",
      }) as HTMLSelectElement;
      return el;
    }

    it("lists every account for an admin, with the current owner selected", async () => {
      authApi.fetchAccounts.mockResolvedValue([
        { user_id: "u-owner", username: "bob", display_name: "Bob Bobson", role: "user" },
        { user_id: "u-nick", username: "nicholas", display_name: "Nicholas", role: "user" },
      ]);
      await renderOpen();
      await screen.findByText("permissionDialog.visibilityOffHint");

      const sel = ownerSelect();
      const labels = Array.from(sel.options).map((o) => o.textContent);
      expect(labels).toContain("Nicholas");
      // The record's current owner is what the select shows, not the first
      // row of the list — a picker that silently defaults is a picker that
      // reassigns on an innocent Save.
      expect(sel.value).toBe("u-owner");
    });

    it("writes the picked account as owner, and nothing else", async () => {
      const { patchNodeOwner, patchNodeVisibility, patchNodeGuests } =
        await import("../../lib/gateway-api");
      authApi.fetchAccounts.mockResolvedValue([
        { user_id: "u-owner", username: "bob", display_name: "Bob Bobson", role: "user" },
        { user_id: "u-nick", username: "nicholas", display_name: "Nicholas", role: "user" },
      ]);
      await renderOpen();
      await screen.findByText("permissionDialog.visibilityOffHint");

      fireEvent.change(ownerSelect(), { target: { value: "u-nick" } });
      // Selecting is a draft: nothing travels until Save.
      expect(patchNodeOwner).not.toHaveBeenCalled();

      fireEvent.click(screen.getByRole("button", { name: "common.confirm" }));
      await waitFor(() =>
        expect(patchNodeOwner).toHaveBeenCalledWith("n-1", "u-nick"),
      );
      // Assigning an owner is one decision. It must not ride along with a
      // visibility or guest-list write the user never made.
      expect(patchNodeVisibility).not.toHaveBeenCalled();
      expect(patchNodeGuests).not.toHaveBeenCalled();
    });

    it("offers no owner control to a non-admin, including the owner themself", async () => {
      // The privilege assertion. `can_attribute` is true for the owner, so
      // gating the picker on `editable` would hand them a control that the
      // Gateway answers with 403.
      ACCOUNT.role = "user";
      try {
        (gatewayApi.fetchNodePermissions as ReturnType<typeof vi.fn>).mockResolvedValue({
          ...PERMS,
          owner_user_id: "me-admin",
        });
        await renderOpen();
        await screen.findByText("permissionDialog.visibilityOffHint");

        expect(
          screen.queryByRole("combobox", { name: "permissionDialog.owner" }),
        ).toBeNull();
        // Still named, still read-only.
        expect(ownerCell().tagName).toBe("SPAN");
        expect(ownerLabel()).toBe("Me");
      } finally {
        ACCOUNT.role = "admin";
      }
    });

    it("clears the owner back to ownerless when the unassigned entry is picked", async () => {
      // The Gateway spells ownerless `null`; the select needs a real option
      // value for it, and "" is that value. Losing this would make an
      // already-owned row impossible to un-assign from the UI.
      const { patchNodeOwner } = await import("../../lib/gateway-api");
      await renderOpen();
      await screen.findByText("permissionDialog.visibilityOffHint");

      fireEvent.change(ownerSelect(), { target: { value: "" } });
      fireEvent.click(screen.getByRole("button", { name: "common.confirm" }));

      await waitFor(() => expect(patchNodeOwner).toHaveBeenCalledWith("n-1", null));
    });
  });

  describe("ownerless resources", () => {
    async function renderOwnerless() {
      const { fetchNodePermissions } = await import("../../lib/gateway-api");
      (fetchNodePermissions as ReturnType<typeof vi.fn>).mockResolvedValue(
        OWNERLESS_PERMS,
      );
      return renderOpen();
    }

    it("offers an admin the claim action, with a hint naming the blocker", async () => {
      await renderOwnerless();
      const claim = await screen.findByRole("button", {
        name: "permissionDialog.claimOwner",
      });
      expect(claim).toBeDefined();
      // `t()` is mocked to echo the key, so the assertion pins the key,
      // not the interpolated prose.
      expect(screen.getByText("permissionDialog.claimOwnerHint")).toBeDefined();
    });

    it("hides the claim action once the resource is owned", async () => {
      const { fetchNodePermissions } = await import("../../lib/gateway-api");
      (fetchNodePermissions as ReturnType<typeof vi.fn>).mockResolvedValue(PERMS);
      await renderOpen();
      await waitFor(() =>
        expect(
          screen.queryByRole("button", { name: "permissionDialog.claimOwner" }),
        ).toBeNull(),
      );
    });

    it("claims ownership for the signed-in admin, then the save succeeds", async () => {
      const { patchNodeOwner, patchNodeVisibility, fetchNodePermissions } =
        await import("../../lib/gateway-api");
      (patchNodeOwner as ReturnType<typeof vi.fn>).mockClear();

      await renderOwnerless();
      const claim = await screen.findByRole("button", {
        name: "permissionDialog.claimOwner",
      });
      fireEvent.click(claim);

      // Claims for the caller themself — the UI never invents an owner id.
      await waitFor(() =>
        expect(patchNodeOwner).toHaveBeenCalledWith("n-1", "me-admin"),
      );

      // After the claim the dialog reloads into the owned state, so the
      // switch is now publishable and the save goes through.
      (fetchNodePermissions as ReturnType<typeof vi.fn>).mockResolvedValue(PERMS);
      const sw = screen.getByRole("switch");
      fireEvent.click(sw);
      await waitFor(() => expect((sw as HTMLInputElement).checked).toBe(true));
      fireEvent.click(screen.getByRole("button", { name: "common.confirm" }));

      await waitFor(() =>
        expect(patchNodeVisibility).toHaveBeenCalledWith("n-1", "public"),
      );
    });

    it("surfaces a failed claim instead of pretending it worked", async () => {
      const { patchNodeOwner } = await import("../../lib/gateway-api");
      (patchNodeOwner as ReturnType<typeof vi.fn>).mockRejectedValueOnce(
        new Error("Failed to update owner: forbidden"),
      );
      await renderOwnerless();
      fireEvent.click(
        await screen.findByRole("button", { name: "permissionDialog.claimOwner" }),
      );
      await waitFor(() => expect(screen.getByText(/forbidden/i)).toBeDefined());
    });

    // The regression that started all this: an admin could flip the
    // switch, press Save, and only then learn (from a 409) that the
    // write was impossible. `can_attribute` is true for an admin, so
    // the permission bit alone cannot drive the disabled state — the
    // data-state bit has to.
    it("disables the visibility switch instead of offering a write that 409s", async () => {
      await renderOwnerless();
      // Wait for the payload to land: the switch exists during the
      // loading state too, when `editable` is still false for a
      // different reason. The hint is the marker that `perms` arrived.
      await screen.findByText("permissionDialog.needsOwnerHint");
      const sw = screen.getByRole("switch") as HTMLInputElement;
      expect(sw.disabled).toBe(true);
    });

    it("keeps an owned resource's switch enabled", async () => {
      // The counterpart: the lock is scoped to the ownerless data state,
      // not to "this caller is an admin" — otherwise the fix would
      // freeze every permission dialog in the product.
      await renderOpen();
      await screen.findByText("permissionDialog.visibilityOffHint");
      const sw = screen.getByRole("switch") as HTMLInputElement;
      expect(sw.disabled).toBe(false);
      expect(screen.queryByText("permissionDialog.needsOwnerHint")).toBeNull();
    });

    it("does not write a visibility the user never touched when claiming", async () => {
      const { patchNodeOwner, patchNodeVisibility } = await import(
        "../../lib/gateway-api"
      );
      (patchNodeOwner as ReturnType<typeof vi.fn>).mockClear();
      (patchNodeVisibility as ReturnType<typeof vi.fn>).mockClear();

      await renderOwnerless();
      fireEvent.click(
        await screen.findByRole("button", { name: "permissionDialog.claimOwner" }),
      );

      await waitFor(() =>
        expect(patchNodeOwner).toHaveBeenCalledWith("n-1", "me-admin"),
      );
      // A bare claim must not smuggle in a visibility write.
      expect(patchNodeVisibility).not.toHaveBeenCalled();
    });

    it("claims ownership without also changing visibility", async () => {
      const { patchNodeOwner, patchNodeVisibility } = await import(
        "../../lib/gateway-api"
      );
      (patchNodeOwner as ReturnType<typeof vi.fn>).mockClear();
      (patchNodeVisibility as ReturnType<typeof vi.fn>).mockClear();

      await renderOwnerless();
      fireEvent.click(
        await screen.findByRole("button", { name: "permissionDialog.claimOwner" }),
      );

      await waitFor(() =>
        expect(patchNodeOwner).toHaveBeenCalledWith("n-1", "me-admin"),
      );
      // An ownerless row is private by construction. Folding "publish it
      // to every logged-in user" into the claim would silently widen who
      // can reach the agent — that call belongs to the admin, made
      // explicitly on the switch afterwards.
      expect(patchNodeVisibility).not.toHaveBeenCalled();
    });
  });
});

// ADR-075 D4: the node display-name field. It is the only control here
// that is not about permissions, and the only one whose draft state
// comes from the DIALOG (the Gateway's permissions payload has no
// `node_name`).
describe("PermissionDialog node display name (ADR-075 D4)", () => {
  function nameInput(): HTMLInputElement {
    const el = document.querySelector<HTMLInputElement>("[data-node-name]");
    if (!el) throw new Error("display name input not found");
    return el;
  }

  async function renderNode() {
    const renamed: Array<[string, string]> = [];
    render(
      <PermissionDialog
        open
        target={NODE}
        onClose={() => {}}
        onRenamedNode={(id, n) => renamed.push([id, n])}
      />,
    );
    await screen.findByRole("switch");
    return renamed;
  }

  it("seeds the input with the node's current name", async () => {
    await renderNode();
    expect(nameInput().value).toBe("My Node");
  });

  it("has no display-name field for an agent", async () => {
    render(
      <PermissionDialog
        open
        target={{ kind: "agent", id: "i-1", name: "An Agent" }}
        onClose={() => {}}
      />,
    );
    await screen.findByRole("switch");
    expect(document.querySelector("[data-node-name]")).toBeNull();
  });

  it("saves a valid rename and reports it to the caller", async () => {
    const { patchNodeName } = await import("../../lib/gateway-api");
    (patchNodeName as ReturnType<typeof vi.fn>).mockClear();
    const renamed = await renderNode();

    fireEvent.change(nameInput(), { target: { value: "gpu-2" } });
    fireEvent.click(screen.getByRole("button", { name: "common.confirm" }));

    await waitFor(() =>
      expect(patchNodeName).toHaveBeenCalledWith("n-1", "gpu-2"),
    );
    // The caller must re-pull the node list, or the group header keeps
    // rendering the pre-rename name.
    expect(renamed).toEqual([["n-1", "gpu-2"]]);
  });

  // The footer is a settings pair, not a browser page: 确认 commits every
  // draft in the form and dismisses; 取消 discards them and dismisses. Same
  // for both targets — there is no reason a node and an agent would differ.
  it("confirms: saves the draft and closes", async () => {
    const { patchNodeVisibility } = await import("../../lib/gateway-api");
    (patchNodeVisibility as ReturnType<typeof vi.fn>).mockClear();
    const closed: string[] = [];
    render(<PermissionDialog open target={NODE} onClose={() => closed.push("node")} />);
    const sw = await screen.findByRole("switch");
    fireEvent.click(sw);
    fireEvent.click(screen.getByRole("button", { name: "common.confirm" }));
    await waitFor(() => expect(patchNodeVisibility).toHaveBeenCalledWith("n-1", "public"));
    await waitFor(() => expect(closed).toEqual(["node"]));
  });

  it("cancels: closes without writing anything", async () => {
    const { patchNodeVisibility } = await import("../../lib/gateway-api");
    (patchNodeVisibility as ReturnType<typeof vi.fn>).mockClear();
    const closed: string[] = [];
    render(<PermissionDialog open target={NODE} onClose={() => closed.push("node")} />);
    const sw = await screen.findByRole("switch");
    fireEvent.click(sw);
    fireEvent.click(screen.getByRole("button", { name: "common.cancel" }));
    expect(closed).toEqual(["node"]);
    expect(patchNodeVisibility).not.toHaveBeenCalled();
  });

  it("titles a node dialog Settings and an agent dialog Permissions", async () => {
    render(<PermissionDialog open target={NODE} onClose={() => {}} />);
    await screen.findByRole("switch");
    expect(screen.getByText("permissionDialog.nodeSettingsTitle")).toBeDefined();
    cleanup();

    render(
      <PermissionDialog
        open
        target={{ kind: "agent", id: "i-1", name: "An Agent" }}
        onClose={() => {}}
      />,
    );
    await screen.findByRole("switch");
    expect(screen.getByText("permissionDialog.title")).toBeDefined();
  });

  it("keeps Save disabled for a slug the Gateway would reject", async () => {
    const { patchNodeName } = await import("../../lib/gateway-api");
    (patchNodeName as ReturnType<typeof vi.fn>).mockClear();
    await renderNode();

    const save = screen.getByRole("button", { name: "common.confirm" });
    for (const bad of ["A", "has space", "double--hyphen", "-lead", "local"]) {
      fireEvent.change(nameInput(), { target: { value: bad } });
      expect((save as HTMLButtonElement).disabled).toBe(true);
    }
    fireEvent.change(nameInput(), { target: { value: "gpu-2" } });
    expect((save as HTMLButtonElement).disabled).toBe(false);

    fireEvent.click(save);
    await waitFor(() => expect(patchNodeName).toHaveBeenCalledTimes(1));
    expect((patchNodeName as ReturnType<typeof vi.fn>).mock.calls[0][1]).toBe("gpu-2");
  });

  it("leaves the draft untouched when the save is unchanged", async () => {
    const { patchNodeName } = await import("../../lib/gateway-api");
    (patchNodeName as ReturnType<typeof vi.fn>).mockClear();
    await renderNode();

    // Re-typing the same value is not a change — a rename must not be
    // issued just because the field was touched.
    fireEvent.change(nameInput(), { target: { value: "My Node" } });
    expect(
      (screen.getByRole("button", { name: "common.confirm" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
    expect(patchNodeName).not.toHaveBeenCalled();
  });
});

// The reported symptom: focus left the field (and the owner's <select>
// popup collapsed, since a native select closes on blur) seconds after the
// user interacted — with nothing refreshing. The cause was NOT the dialog
// re-rendering on its own data; it was the PARENT re-rendering for
// unrelated reasons (MQTT ticks, inventory re-broadcasts, the 3 s gateway
// death-watch probe), handing the inline `onClose` a new identity, which
// re-ran the effect that called `closeRef.current?.focus()`.
//
// So the test drives the real thing: a parent that re-renders with a
// FRESH onClose, which is what every AgentList tick looks like.
describe("PermissionDialog focus survival", () => {
  /** Wraps the dialog in a parent whose `onClose` identity changes on
   *  every render — the inline-arrow pattern used throughout AgentList.
   *  `onCloseTick` stands in for the unrelated work that makes AgentList
   *  re-render (an MQTT tick, a re-published inventory). */
  function Wrapped({ onCloseTick }: { onCloseTick: () => void }) {
    const [, bump] = useState(0);
    return (
      <>
        <button onClick={() => { bump((n) => n + 1); onCloseTick(); }}>tick</button>
        <PermissionDialog open target={NODE} onClose={() => {}} />
      </>
    );
  }

  it("keeps focus in the display-name input across unrelated parent re-renders", async () => {
    render(<Wrapped onCloseTick={() => {}} />);
    const input = await waitFor(() => {
      const el = document.querySelector<HTMLInputElement>("[data-node-name]");
      if (!el) throw new Error("display name input not found");
      return el;
    });
    input.focus();
    expect(document.activeElement).toBe(input);

    // Three "unrelated store tick" re-renders, each rebuilding the inline
    // `onClose` — exactly what the effect's old dep array keyed on.
    fireEvent.click(screen.getByRole("button", { name: "tick" }));
    fireEvent.click(screen.getByRole("button", { name: "tick" }));
    fireEvent.click(screen.getByRole("button", { name: "tick" }));

    // Focus must still be where the user left it.
    expect(document.activeElement).toBe(input);
  });

  it("keeps focus on the owner <select> across unrelated parent re-renders", async () => {
    render(<Wrapped onCloseTick={() => {}} />);
    await screen.findByRole("switch");
    const select = ownerCell();
    select.focus();
    expect(document.activeElement).toBe(select);

    fireEvent.click(screen.getByRole("button", { name: "tick" }));
    fireEvent.click(screen.getByRole("button", { name: "tick" }));

    expect(document.activeElement).toBe(select);
  });

  it("still focuses the close button when the dialog OPENS", async () => {
    render(<Wrapped onCloseTick={() => {}} />);
    await screen.findByRole("switch");
    // The initial focus is deliberate — the dialog must be keyboard-ready.
    // It just must not RE-fire on every later render.
    const closeBtn = document.querySelector<HTMLButtonElement>('[aria-label="permissionDialog.ariaLabelClose"]');
    expect(closeBtn).not.toBeNull();
    expect(document.activeElement).toBe(closeBtn);
  });
});
