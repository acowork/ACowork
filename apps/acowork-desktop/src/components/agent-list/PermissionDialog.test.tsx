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
  role: "admin" as "admin" | "user",
  display_name: "Me",
};

vi.mock("../../stores/authStore", () => ({
  useAuthStore: {
    getState: () => ({ accessToken: "t", account: ACCOUNT }),
  },
}));

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { PermissionDialog, type PermissionTarget } from "./PermissionDialog";
import * as gatewayApi from "../../lib/gateway-api";
import * as authApi from "../../lib/auth-api";

const NODE: PermissionTarget = { kind: "node", id: "n-1", name: "My Node" };

/** The owner cell — carries `data-owner`, so it stays addressable when a
 *  guest row shows the same person (the roster legitimately lists the
 *  owner too). */
function ownerCell(): HTMLElement {
  const el = document.querySelector<HTMLElement>("[data-owner]");
  if (!el) throw new Error("owner cell not found");
  return el;
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
    const save = screen.getByRole("button", { name: "permissionDialog.save" }) as HTMLButtonElement;
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

    const save = screen.getByRole("button", { name: "permissionDialog.save" });
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
      expect(ownerCell().title).toBe("Bob Bobson");
      expect(ownerCell().textContent).not.toBe("u-owner");
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
      expect(ownerCell().title).toBe("Me");
      expect(ownerCell().textContent).not.toBe("me-admin");
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
      expect(ownerCell().textContent).toBe("u-owner");
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
      fireEvent.click(screen.getByRole("button", { name: "permissionDialog.save" }));

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
