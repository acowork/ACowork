# Mobile App

> Version: v1.0 | Updated: 2026-10-02
> Status: design confirmed, engineering not yet scaffolded (v1 is a UI/interaction design — see `docs/prototypes/mobile-im-v1.html`)

---

ACowork Mobile App is a Tauri v2 based mobile client, positioned as a **portable operating terminal** for ACowork. It is not a port of the Desktop App — it is a second terminal with a **reduced feature set and a different shape**.

## 1. Positioning and Responsibilities

### 1.1 In one sentence

**Mobile App = an IM-shaped conversation control terminal**: it carries "conversations with Agents and colleagues" using the interaction paradigm of mainstream IM software (WeCom / DingTalk), does basic conversation control in v1, and takes on no platform-management duties.

### 1.2 Relationship to Desktop App

| Dimension | Desktop App | Mobile App |
|-----------|-------------|------------|
| Positioning | Primary workbench — create/debug/publish Agents | Portable terminal — receive and send messages anywhere |
| Shape | Left/center/right multi-column | Bottom tab bar + full-screen hierarchy (IM shape) |
| Navigation | Single nav bar + left list | One independent stack per tab + iOS push semantics |
| Sessions | An Agent may have **multiple Session Tabs open in parallel** | An Agent has multiple sessions, **switched serially** (see §5) |
| Agent management | Create/install/clone/publish | Not in v1 |
| Debugging | DevMode protocol, Git status bar | Not in v1 |
| Local Gateway | Embeds/manages a Gateway process | **No local Gateway process** (see §9.1) |

**Key insight**: the two Apps share the same server-side state (same Agents, same Sessions, same permission model), but **their client state is independent**. A session opened on Desktop can be continued on the phone; a session created on the phone is visible on Desktop (subject to permissions). Mobile is never a "remote display" for Desktop — it is a peer terminal.

### 1.3 What Mobile App does not do (v1)

| Not doing | Why |
|------------|-----|
| DevMode debugging protocol | Requires local filesystem and process-level control; a phone has neither |
| Git status bar | Depends on live workspace fs watching (ADR-078); no local workspace on mobile |
| Agent create/install/clone/publish | Occasional developer operations — stay on desktop |
| Provider / API Key / MCP / Embedding management | Harness's job, involves secrets; a phone should not hold them |
| Rich-text editor | Document editing needs a large screen and a physical keyboard |
| Harness / Extensions navigation | Developer concepts; ordinary users do not need them |
| Local Gateway process management | Mobile does not run a Gateway (§9.1) |

> **Note**: the table above means "not in v1", not "never". Multi-user collaborative document editing (Yjs, ADR-079) is evaluated after v2.

## 2. Information Architecture (IA)

### 2.1 Bottom Tab Bar (fixed, four items)

```
┌─────────────────────────────────┐
│  ← Back      ADR-085…      ⌄  ⋯ │  ← detail page: tab bar slides away
│                                  │
│         (content)                │
│                                  │
├─────────────────────────────────┤
│  ┌──────┬──────┬──────┬──────┐  │
│  │ Chat │Tasks │ Docs │Config│  │  ← root pages only
│  └──────┴──────┴──────┴──────┘  │
└─────────────────────────────────┘
```

| Tab | Desktop counterpart | Notes |
|-----|--------------------|-------|
| Chat | AgentList + UserList + ChatPanel | Primary entry point; ~80% of daily use |
| Projects | ProjectsView | Read-only board + task transitions |
| Documents | DocsView | Directory tree + document reading + review |
| Settings | SettingsPage | Secondary settings pages |

**Harness and Extensions do not enter mobile navigation.** The settings page keeps greyed-out "desktop only" entries as placeholders, so users know the features exist and are available elsewhere — honesty beats silence.

### 2.2 Navigation model: one independent stack per tab

```
Tab stacks (in-memory; depth preserved across tab switches)
chat:     [Conversation list] → [Chat detail]
projects: [Project list] → [Board] → [Task detail]
docs:     [Document tree] → [Document content] → [Review detail]
settings: [Settings root] → [Profile|General|Appearance|Gateway]
```

| Rule | Behavior |
|------|----------|
| Enter a detail page | The bottom tab bar **slides away entirely** (iOS / WeChat behavior) |
| Switch tab | Each tab keeps its own stack depth; leaving a chat and returning lands you in the same chat |
| Tap the current tab again | Return to that tab's root (clear the stack) |
| Back | Pop one level; the tab bar slides back in |

**Why the tab bar hides on detail pages**: neither iOS nor WeCom shows a bottom bar on a second-level page. Showing both leaves the user facing two navigation exits — bottom tabs and the top-right back button — with no guidance on which to use. Hiding it leaves exactly one back path (top-right, or the right-swipe gesture).

### 2.3 Chat home = a unified conversation stream

Desktop has two separate lists (AgentList and UserList). Mobile merges them into a **single IM conversation stream**:

```
┌─────────────────────────────────┐
│  🔍 Search agents or contacts     │
├─────────────────────────────────┤
│  AGENTS (7)                      │
│  ┌───┬──────────────────────┐   │
│  │ ● │ Architect      14:32  │   │
│  │   │ ADR-085 state machine…│ 2 │
│  └───┴──────────────────────┘   │
│  ┌───┬──────────────────────┐   │
│  │ ● │ Senior Eng.    13:05  │   │
│  │   │ cargo test all green…│   │
│  └───┴──────────────────────┘   │
│  CONTACTS (4)                    │
│  ┌───┬──────────────────────┐   │
│  │ ● │ Alice            15:10│  │
│  │   │ embedding, 4 dims…   │ 3 │
│  └───┴──────────────────────┘   │
└─────────────────────────────────┘
```

Each row shows: avatar (with presence dot), name, online status, **most recent conversation summary**, timestamp, unread badge.

**The summary comes from the last message of the most recently active session** — not a fixed Agent-level string. IM semantics demand it: what the user wants to know is "where did we leave off".

## 3. Gestures and Navigation Interaction

| Gesture | Scope | Behavior |
|---------|-------|----------|
| Swipe left → | Chat detail page | Open the Agent settings drawer (Desktop RightPanel) |
| Swipe right ← | Detail page | Go back one level (pop) |
| Swipe right ← | Root page | **No action** |
| Vertical swipe | Anywhere | Yields to page scrolling (does not trigger navigation) |
| Horizontal swipe | Detail page top (horizontal list areas) | Yields to the child component |

**Detection rule**: a horizontal gesture is recognized only when `|dx| > |dy|` and `|dx| > 6px`; the commit threshold is 38% of screen width or sufficient velocity (<300ms). The chat page must resolve between "swipe left opens drawer" and "swipe right goes back" based on the **actual direction** — this is the branch most easily implemented wrong.

## 4. Chat Detail Page

### 4.1 Layout

```
┌─────────────────────────────────┐
│  ←     ADR-085 Lifecycle SM  ⌄  ⋯ │  ← title = session switcher
│        gpt-5 · 24 messages       │
├─────────────────────────────────┤
│  Today 14:02                     │
│                                  │
│              ┌─────────────┐    │
│              │ help me look│    │  ← my message (right)
│              └─────────────┘    │
│  ┌──────────────────────────┐   │
│  │ ⚡ file_read  412ms       │   │  ← tool-call card
│  │ docs/adr/zh/ADR-085…      │   │
│  └──────────────────────────┘   │
│  ┌─────┐                        │
│  │ Arch│ two ambiguities: 1)  │  │  ← Agent message (left)
│  └─────┘ rollback 2) RESTART… │   │
│                                  │
│  ┌──────────────────────────┐   │
│  │ ⚠ Confirm: run shell     │   │  ← approval card
│  │ cargo test -p …          │   │
│  │        [Deny]  [Allow]   │   │
│  └──────────────────────────┘   │
├─────────────────────────────────┤
│ [⚙] [Send a message…      ] [🌐] [🙂] [↑] │
└─────────────────────────────────┘
```

### 4.2 Agent settings drawer (swipe left)

Corresponds to Desktop's RightPanel, with six sections: Status / Workspace / Memory / Tools / Config / Sessions.

Mobile uses **native-feeling components** (inset group lists, Switch, Segmented, Stepper, select dropdowns, Action Sheet) rather than scaled-down Desktop controls.

**The workspace section offers Add to Chat only — no file preview.** Mobile has no Monaco, and file content is better read by the Agent inside the conversation. This boundary also honors [ADR-009 §5](../../adr/zh/ADR-009-gateway-workspace-isolation.md): mobile never touches the filesystem directly, going only through the Gateway reverse proxy.

### 4.3 Message elements

| Element | Mobile treatment |
|---------|------------------|
| User / Agent message bubbles | Kept, with Markdown rendering |
| Tool calls | Collapsed card, collapsed by default (narrow screen) |
| Approval card | Full action buttons kept (high-frequency on mobile) |
| AskQuestion card | Option buttons kept |
| Streaming output | Incremental rendering kept |
| Code blocks | Horizontal scroll, **no syntax highlighting** (no Monaco) |
| Image / file attachments | Read-only display, no previewer |
| Think / Compaction cards | Collapsed to a one-line summary |

## 5. Multi-Session Model (core design)

> The first interaction prototype suggested "one active session per Agent". That was **rejected**. Multiple sessions per Agent is a business fact; Mobile is only one of several operating terminals — dropping multi-session would make the product unusable.

### 5.1 Why one session per Agent is not acceptable

An Agent being driven by several topics at once is normal: the architect is writing an ADR in one session, reviewing someone else's PR in another, and debugging a production issue in a third. Desktop carries this with **parallel Session Tabs**. A 390pt-wide phone screen cannot fit N side-by-side tabs — but that is a reason **"tabs are the wrong container"**, not a reason **"multiple sessions are not needed"**.

### 5.2 Container: the nav-bar title is the session switcher

```
┌─────────────────────────────────┐
│  ←      ADR-085 Lifecycle SM   ⌄  ⋯ │
│        gpt-5 · 24 messages       │  ← tap here
└─────────────────────────────────┘
              ↓ tap
┌─────────────────────────────────┐
│      Architect · Sessions (8/12)  │
│  Loaded the latest 8; scroll for more│
├─────────────────────────────────┤
│ ✓ ADR-085 Lifecycle Review    🌐  │
│   ADR-085 state machine review…  │
│ ─────────────────────────────────  │
│   Relay Degradation List     🌐 RO│
│   node-local degradation table…  │
│ ─────────────────────────────────  │
│   ADR-084 User Process        🔒  │
│   ADR-084 finalized, awaiting…   │
│ ─────────────────────────────────  │
│   Vault Key Rotation         🌐 RO│
├─────────────────────────────────┤
│        ＋ New session              │
│        Manage all sessions         │
│        [   Cancel   ]              │
└─────────────────────────────────┘
```

| Design point | Rationale |
|--------------|-----------|
| Title shows `session.title` | Not `agent.name` — users relate to topics, not bots |
| Each row shows 🌐/🔒 | Visibility marker (§6) |
| Read-only rows show a "read-only" badge | States clearly that writing is unavailable |
| ✓ marks the current session | Positional awareness |
| Scrollable list + "8/12" pagination hint | Mirrors `agentStore.fetchSessions(agentId, page)` |
| "Manage all sessions" at the bottom | Jumps to the drawer's Sessions section (bulk management) |

### 5.3 Switching sessions is an atomic transition

Switching is **not** a local page change; it must follow the full flow (mirroring Desktop's `chatStore.openSession`):

```
User taps a session row
      │
      ├─→ ① Frontend switches the current session + reloads message history
      ├─→ ② POST /api/agents/{id}/sessions/{sid}/open (ADR-038 activation, idempotent)
      └─→ ③ In parallel: fetch session config + state
```

**Read-only sessions skip ②.** Activation is per-session global lifecycle state: a viewer has no right to activate (close is write-gated on purpose, and the owner cannot see that anyone is holding it), and read-only viewing does not need activation — history comes from `GET /messages`, and events arrive over the wildcard MQTT subscription whenever the session is genuinely Active (i.e. while its owner has it open).

### 5.4 Session management (drawer → Sessions section)

| Capability | Supported | Notes |
|------------|-----------|-------|
| List | ✅ | Paginated, "8/12" form, can load more |
| Create | ✅ | Defaults to Private (§6.4) |
| Switch | ✅ | Equivalent to §5.2 |
| Delete | ✅ | Writable sessions only (owner/admin) |
| Rename | ❌ v1 | Requires desktop |
| Toggle visibility | ✅ | The 🌐/🔒 control in the composer |

## 6. Multi-User Permission Model

> The full derivation is in [ADR-076](../../adr/zh/ADR-076-multi-user-account-system.md) §决策 4. This section defines only how mobile **consumes** it.

### 6.1 The backend's `can_write` is the single source of truth

```rust
// core/acowork-runtime/src/conversation.rs
pub struct SessionListView {
    pub session_id: String,
    pub visibility: Option<SessionVisibility>,  // public / private
    pub can_write: bool,                        // ← the only authority
}
```

**Mobile must consume `can_write` as-is and must never re-derive write permission from the `visibility` label.** Public means everyone can read, but normally only the owner can write. A 🌐 public session can perfectly well be read-only.

The predicate (isomorphic to Desktop's `isReadOnlySession`):

```ts
// Read-only applies only when can_write === false; a missing field means writable.
// The degradation direction is "let the backend reject" rather than "lock every control":
// an optimistically created session, or an older Runtime that omits the field,
// should not leave the user facing a row of dead buttons.
function isReadOnly(canWrite: boolean | undefined): boolean {
  return canWrite === false;
}
```

### 6.2 Visibility / writability matrix

| scope \ session | public | private |
|---|---|---|
| admin | read + write | read + write |
| owner | read + write | read + write |
| other user | **read (read-only)** | **invisible** (404) |
| local (headerless) | read + write | read + write |

Mapping to mobile UI:

| State | Mobile behavior |
|-------|----------------|
| Writable | Normal composer + 🌐/🔒 toggle |
| Read-only (`can_write === false`) | Read-only banner on top + approval/AskQuestion cards hidden + composer replaced by a read-only line |
| Invisible (private, not owner) | The session never appears in the list (the backend filters before pagination) |

### 6.3 Concrete handling of the read-only state

```
┌─────────────────────────────────┐
│ 🔒 Read-only · shared by Alice; you cannot send messages │
├─────────────────────────────────┤
│           (message list, read-only)│
├─────────────────────────────────┤
│ 🔒 Shared by Alice · read-only   │  ← replaces the textarea
└─────────────────────────────────┘
```

| Control | When read-only | Why |
|---------|----------------|-----|
| textarea | **not rendered** | A placeholder that looks typeable is a worse experience |
| Send / tools / attachments | not rendered | No write path exists |
| Approval / AskQuestion cards | **hidden** | They would make a viewer decide on the owner's behalf |
| Visibility icon | **disabled, not hidden** | A viewer still needs to know what they are looking at |
| Change visibility / delete | Menu items disabled + reason sub-label | Not "click, then get a toast" |

**Why the visibility icon is disabled rather than hidden**: this matches Desktop's existing `SessionVisibilityToggle` rule — a viewer must be able to see that what they are reading is a private session. Hiding the icon would strip them of that information.

### 6.4 Default visibility for new sessions

New sessions default to **Private**, matching ADR-076's `create_frontend_session`, which writes `Some(Private)` alongside `user_id`. Mobile submits private up front, avoiding any "newly created but publicly readable by bob" window.

## 7. Projects and Documents

| Module | Hierarchy | v1 degradation |
|--------|-----------|----------------|
| Projects | List → board → task detail | **Board columns degrade to a horizontal status Chip filter** (no drag-and-drop board on a narrow screen) |
| Documents | Directory tree → document content → review detail | Read-mostly; no rich-text editing |

All detail pages support right-swipe to go back one level.

## 8. Settings

Four top-level entries, each opening a secondary settings page:

| Top level | Content | v1 status |
|-----------|---------|-----------|
| Profile | Account, avatar, role | ✅ |
| General | Language, notifications, defaults | ✅ |
| Appearance | Theme (light/dark/system), accent color | ✅ |
| Gateway | Connection address, mode | ⚠️ **Degraded** (see §9.1) |

## 9. Runtime and Platform Constraints

### 9.1 Mobile has no local Gateway process

Desktop embeds and manages a local Gateway process, hence settings for "start/stop/restart Gateway". **Mobile has no such process** — it connects over HTTP/MQTT to an already-running Gateway (usually the user's own machine, or a relay deployment).

Therefore in v1:
- Start / stop / restart Gateway → **disabled**
- Gateway address configuration → **kept** (this is mobile's most critical connection setting)
- Gateway status display → ✅ kept

### 9.2 Technology Choices

| Item | Choice | Rationale |
|------|--------|-----------|
| Framework | Tauri v2 | Same stack as Desktop; shares HTTP/MQTT clients and type definitions |
| Frontend | React + TypeScript | Same as Desktop; can reuse pure logic such as `session-control.ts` |
| Styling | CSS Modules / vanilla CSS | Mobile feel needs fine-grained control; Tailwind's Desktop breakpoint model does not fit |
| Components | Custom mobile component set | Inset lists / Switch / Segmented / Stepper / Action Sheet |
| State | Reuse the Desktop store pattern (Zustand) | Same concepts, low migration cost |

**The frontend codebase is NOT shared with Desktop.** The two Apps have entirely different information architecture, components, and interaction paradigms; forcing a shared codebase accumulates a large number of conditional branches. What is shared is the **protocol-layer type definitions** (`SessionInfo`, the `can_write` semantics).

## 10. Relationship to Existing Documents

| Document | Relationship |
|----------|--------------|
| [01-overview.md](./01-overview.md) | Concretizes §5 "deep mobile adaptation" |
| [14-desktop-app.md](./14-desktop-app.md) | Mobile is a subset terminal of Desktop; shared backend contract |
| [21-pm-project-management.md](../../design/zh/21-pm-project-management.md) (zh) | Data source for the Projects tab |
| [20-doc-online-document.md](../../design/zh/20-doc-online-document.md) (zh) | Data source for the Documents tab |
| [ADR-009](../../adr/zh/ADR-009-gateway-workspace-isolation.md) | Basis for the Add-to-Chat-only workspace boundary |
| [ADR-076](../../adr/zh/ADR-076-multi-user-account-system.md) | **Authoritative** definition of session visibility and writability |
| [ADR-085](../../adr/zh/ADR-085-agent-lifecycle-state-machine.md) | State preconditions for session activation |

## 11. Design Decision Log

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Shape | IM style, not a direct three-column translation | Mobile is "messages anywhere", not "managing many panels" |
| Bottom tabs | 4 (no Harness/Extensions) | Developer concepts do not belong in an ordinary user's navigation |
| Conversation list | Agents + contacts merged into one stream | The IM mental model is "find someone to talk to", not "pick an Agent" |
| Navigation stacks | One independent stack per tab | Preserves context across tab switches, matching IM habits |
| Tab bar on detail pages | Hidden | Avoids two coexisting navigation systems |
| Multi-session container | Nav-bar title + Action Sheet | A narrow screen cannot fit a side-by-side tab bar |
| Session summary source | Last message of the most recently active session | IM semantics |
| Write-permission source | Backend `can_write`, never derived locally | Public ≠ writable; client-side derivation creates a second source of truth |
| Read-only textarea | Not rendered | A fake typeable placeholder is a worse experience |
| Read-only visibility icon | Disabled, not hidden | A viewer needs to know what they are looking at |
| New session visibility | Defaults to Private | Matches ADR-076's create path |
| Workspace | Add to Chat only, no preview | No Monaco; the Agent reading files is more natural |
| Board | Degrades to Chip filters | Drag-and-drop is unusable on a narrow screen |
| Gateway process management | Disabled in v1 | Mobile runs no local Gateway process |
| Frontend code reuse | Protocol-layer types only | IA and components differ entirely; sharing would accumulate conditionals |

## 12. Deliverables

| Phase | Deliverable | Status |
|-------|-------------|--------|
| Interaction design | [`docs/prototypes/mobile-im-v1.html`](../../prototypes/mobile-im-v1.html) | ✅ Done (single-file HTML, no build step) |
| Architecture decision | [ADR-086](../../adr/zh/ADR-086-mobile-app-im-ia-and-multi-session.md) (zh) | ✅ Done |
| Engineering scaffold | [`apps/acowork-mobile`](../../../apps/acowork-mobile) | ✅ Done (Vite + React 19 + Tauri v2) |
| Protocol-layer reuse | Shared type-definition crate | Planned |

### 12.1 Scaffold Technology Choices

| Area | Choice | Rationale |
|------|--------|-----------|
| Shell | Tauri v2 (standalone crate, as Desktop) | Reuses the core crates; webview first paint and memory beat RN |
| Frontend | Vite + React 19 + zustand + TypeScript strict | Same stack as Desktop, but **no shared code** |
| Styling | Plain CSS + design tokens, no UI framework | The mobile-native component set is "few and fixed"; a framework's defaults all have to be overridden anyway |
| Native layer | Near-empty shell | No local Gateway, no tray, no LSP sidecar — see the "Gateway process management" row in §11 |
| Mobile crate shape | Empty `[workspace]` table, resolved standalone | Same as `apps/acowork-desktop/src-tauri`; mobile targets a different platform and must not inherit core's feature unification |

**Directory layout**

```
apps/acowork-mobile/
  src/
    lib/       types.ts (wire-format subset) · session-write-access.ts (permission gate) · api.ts (HTTP transport)
    stores/    navStore (four independent tab stacks) · agentStore (directory + sole owner of the session list) · chatStore (view state + atomic openSession)
    components/ ui.tsx (ListSection/Row · Switch · Segmented · Stepper · Sheet) · EdgeSwipe.tsx
    screens/chat/  ChatListScreen · ChatDetailScreen
    routes.tsx  route registry
  src-tauri/    standalone crate, near-empty shell
```

### 12.2 Architectural Defect Found While Scaffolding

`agentStore` and `chatStore` each held their own `sessions` array. `session-write-access.ts` read the former; `chatStore.openSession` read the latter (always empty). So `can_write` could never be observed as `false` — read-only sessions would wrongly emit `open_session`, and the composer gate would fail open.

This is not a "sync both sides" fix: **two copies of one fact will always diverge**, and the divergence direction here is exactly a fail-open on the permission gate. The fix gives `agentStore` sole ownership of the session list; `chatStore` keeps only per-agent view state (current session, messages, loading) and reads `can_write` through `getSession()`. The defect was surfaced by a delete-case test in `session-access.test.ts`.

**Corollary: whatever field the permission gate reads must have exactly one owner.**

### 12.3 Prototype Verification Record

The interaction prototype was validated in two automated passes:

| Layer | Tool | Cases | Result |
|-------|------|-------|--------|
| Behavior | jsdom (real clicks + synthetic touch events) | 60 | 60 passed |
| Layout | WKWebView live `getBoundingClientRect` | 44 | 44 passed |

The layout layer cannot be skipped: jsdom returns 0 for every geometry value and would mask real overflow. The prototype verification surfaced 4 real defects (nav-bar subtitle never rendered, session switcher bursting the nav bar, session list never scrolling because `max-height` was too generous, session names rendered twice).
