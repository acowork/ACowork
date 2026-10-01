/**
 * capsuleLayout — regression test for the unified capsule layout.
 *
 * Why this file exists:
 *   User-visible feature: every top-level pane (agent list, chat surface,
 *   right panel, file editor, and the pm / docs / extensions views) wears
 *   the same rounded "capsule" shell on the window's vibrancy, and it paints
 *   from the first frame — including the states where nothing is selected
 *   (no agent installed, no project, no doc).
 *
 *   Before the fix each view hardcoded its own container classes: chat wore
 *   floating capsules over a transparent wrapper, while pm / docs /
 *   extensions painted a solid `bg-page-bg` plane with no panel outlines.
 *   With no agent installed only the left AgentList capsule was visible and
 *   the whole right side read as undifferentiated glass.
 *
 * What this pins:
 *   1. CAPSULE_PANE_CN still carries the shape (rounded + hairline border +
 *      the min-h-0 that lets inner scroll roots work).
 *   2. The four pre-existing chat-side panels derive their shell from the
 *      constant instead of re-spelling the classes.
 *   3. EmptyChatPane / EmptyRightPane render the shell, so the empty states
 *      are outlined panels rather than bare text on glass.
 *   4. The pm / docs / extensions view wrappers no longer paint a solid
 *      `bg-page-bg` plane — the vibrancy must show through, which is what
 *      makes the capsule language read as one system.
 *
 * Why source scanning instead of rendered DOM:
 *   jsdom computes no layout, and several of these panes (AppLayout, the pm
 *   and doc views) pull in Tauri / MQTT / monaco stores that make a render
 *   assertion both brittle and unrepresentative. The class strings ARE the
 *   spec here — same approach as markdownTable.roundedCorners.test.tsx.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { CAPSULE_PANE_CN } from "./capsule";
import { EmptyChatPane, EmptyRightPane } from "../chat/EmptyChatPane";
import { ExtensionsView } from "../../views/ExtensionsView";

const __dirname = dirname(fileURLToPath(import.meta.url));

const read = (rel: string) => readFileSync(resolve(__dirname, "..", "..", rel), "utf8");

describe("CAPSULE_PANE_CN", () => {
  it("keeps the capsule shape", () => {
    expect(CAPSULE_PANE_CN).toContain("rounded-xl");
    expect(CAPSULE_PANE_CN).toContain("border-border-outer");
    expect(CAPSULE_PANE_CN).toContain("overflow-hidden");
    // Without min-h-0 a flex-col child with tall content pushes its inner
    // scroll roots out of the panel instead of letting them scroll.
    expect(CAPSULE_PANE_CN).toContain("min-h-0");
  });
});

describe("chat-side panels share the capsule shell", () => {
  const consumers = [
    "components/chat/ChatPanel.tsx",
    "components/right-panel/RightPanel.tsx",
    "components/editor/FileEditorPanel.tsx",
    "components/layout/AppLayout.tsx",
  ];

  it.each(consumers)("%s uses the shared constant", (rel) => {
    expect(read(rel)).toContain("CAPSULE_PANE_CN");
  });

  it("no longer re-spells the panel shell inline", () => {
    // A hand-written `rounded-xl ... border-border-outer` shell is how the
    // views drifted apart in the first place — one more copy reintroduces
    // the split this constant exists to remove.
    for (const rel of [
      "components/chat/ChatPanel.tsx",
      "components/right-panel/RightPanel.tsx",
      "components/editor/FileEditorPanel.tsx",
    ]) {
      expect(read(rel)).not.toMatch(
        /className="[^"]*rounded-xl[^"]*border-(border-outer|right-panel-border)/,
      );
    }
  });
});

describe("empty placeholders are outlined capsules", () => {
  it("EmptyChatPane renders the chat-body capsule", () => {
    const { container } = render(<EmptyChatPane />);
    const shell = container.firstElementChild as HTMLElement;
    expect(shell.className).toContain("rounded-xl");
    expect(shell.className).toContain("border-border-outer");
    expect(shell.className).toContain("bg-chat-body");
  });

  it("EmptyRightPane reserves the right panel's width and surface", () => {
    const { container } = render(<EmptyRightPane width={340} />);
    const outer = container.firstElementChild as HTMLElement;
    expect(outer.style.width).toBe("340px");
    const shell = outer.firstElementChild as HTMLElement;
    expect(shell.className).toContain("rounded-xl");
    expect(shell.className).toContain("bg-right-panel");
    // Purely a frame — nothing to announce to a screen reader.
    expect(outer.getAttribute("aria-hidden")).toBe("true");
  });
});

describe("pm / docs / extensions join the capsule layout", () => {
  it.each([
    ["views/ProjectsView.tsx", "projects"],
    ["views/DocsView.tsx", "docs"],
    ["views/ExtensionsView.tsx", "extensions"],
  ])("%s no longer walls off the vibrancy (%s)", (rel) => {
    const src = read(rel);
    expect(src).toContain("CAPSULE_PANE_CN");
    // The AppLayout wrapper is transparent now; a leftover solid plane here
    // would cover the vibrancy again and hide the capsules.
    expect(src).not.toMatch(/rounded-xl bg-page-bg/);
  });

  it("AppLayout keeps settings / harness on their solid page", () => {
    const src = read("components/layout/AppLayout.tsx");
    // settings + harness are modal-surface card flows — a deliberate
    // different language, so their solid plane must stay.
    expect(src.match(/rounded-xl bg-page-bg/g)?.length).toBe(2);
  });

  it("each view root fills its wrapper", () => {
    // Regression: DocsView's root was `flex h-full` with no `w-full`, so as
    // an AppLayout flex item it sized itself to its CONTENT. With no doc
    // open that collapsed the layout to the tree's width and left a wide
    // strip of bare vibrancy where the editor capsule should be (Extensions
    // View had the same defect). The bug predates the capsule work — two
    // identical `bg-page-bg` planes hid it — and jsdom computes no layout,
    // so pin the class on the root div (the first `flex h-full` in each
    // file) instead.
    //
    // ExtensionsView's root may carry extra classes between `h-full` and
    // `w-full` (it has `min-h-0` to let the two-column split shrink), so
    // match the two tokens independently rather than one exact string.
    for (const rel of [
      "views/ProjectsView.tsx",
      "views/DocsView.tsx",
      "views/ExtensionsView.tsx",
    ]) {
      const root = read(rel).match(/<div className="flex h-full[^"]*"/);
      expect(root, `${rel} has no <div className="flex h-full" root`).not.toBeNull();
      expect(root![0], `${rel} root must carry w-full`).toContain("w-full");
    }
  });
});

describe("extensions view wears the two-column capsule layout", () => {
  it("renders both columns as capsules", () => {
    // The list sidebar and the detail placeholder must both draw the shared
    // shell — a bare div on either side is exactly the split the constant
    // exists to prevent.
    const { container } = render(<ExtensionsView />);

    const sidebar = container.querySelector("aside");
    expect(sidebar, "no <aside> sidebar").not.toBeNull();
    for (const cn of CAPSULE_PANE_CN.split(" ")) {
      expect(sidebar!.className, `sidebar missing "${cn}"`).toContain(cn);
    }
    expect(sidebar!.className).toContain("bg-nav-surface");

    const detail = container.querySelector("#extensions-detail");
    expect(detail, "no detail placeholder").not.toBeNull();
    for (const cn of CAPSULE_PANE_CN.split(" ")) {
      expect(detail!.className, `detail missing "${cn}"`).toContain(cn);
    }
    // The detail column is a CONTENT main area, so it takes `bg-page-bg` —
    // the same surface as ProjectBoard and DocEditor. `bg-right-panel` is the
    // chat view's 6-tab inspector column (a step darker than page-bg); using
    // it here would make the detail column darker than the list beside it and
    // read as a demoted panel. The extensions view is list+detail like pm/doc,
    // not body+inspector like chat, so the surface split follows pm/doc.
    expect(detail!.className).toContain("bg-page-bg");
    expect(detail!.className).not.toContain("bg-right-panel");
  });

  it("gives the two columns the pm / doc surface split", () => {
    // Left list and right detail must differ, and each must match the view it
    // was copied from: ProjectSidebar (nav-surface) + ProjectBoard (page-bg),
    // DocTreeSidebar (nav-surface) + DocEditor pane (page-bg). One line, but
    // it is the whole "these two panes read as one system" claim.
    const { container } = render(<ExtensionsView />);
    const sidebar = container.querySelector("aside")!.className;
    const detail = container.querySelector("#extensions-detail")!.className;
    expect(sidebar).toContain("bg-nav-surface");
    expect(detail).toContain("bg-page-bg");
    expect(sidebar).not.toBe(detail);
  });

  it("keeps the search box outside the scrolling list", () => {
    // Search pinned above, list scrolls on its own. If the search box were
    // inside the scroll root it would scroll away with a long list.
    const { container } = render(<ExtensionsView />);
    const input = screen.getByRole("textbox");
    const list = screen.getByRole("listbox");
    expect(list.contains(input), "search box must not be inside the listbox").toBe(false);
    expect(list.className).toContain("overflow-y-auto");
    expect(list.className).toContain("min-h-0");
    expect(list.className).toContain("flex-1");
  });

  it("shows an empty placeholder, never fake extension rows", () => {
    // The Gateway extensions endpoint doesn't exist yet, so the list is
    // empty by construction. Pinning it: a demo row here would make it
    // impossible to tell real data from placeholder data in review.
    const { container } = render(<ExtensionsView />);
    expect(screen.queryAllByRole("option")).toHaveLength(0);
    expect(container.textContent).toContain("No extensions available yet.");
  });

  it("links the list to the detail column", () => {
    const { container } = render(<ExtensionsView />);
    expect(screen.getByRole("listbox").getAttribute("aria-controls")).toBe("extensions-detail");
    expect(container.querySelector("#extensions-detail")?.getAttribute("aria-label")).toBeTruthy();
  });
});
