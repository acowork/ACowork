# ADR-086: Mobile App Information Architecture (IM Form) and Multi-Session Carrier

> **Chinese source of truth**: [ADR-086](../zh/ADR-086-mobile-app-im-ia-and-multi-session.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Accepted (2026-10-02; the UI/interaction design passed two-layer automated verification; the
engineering build has not started)

## Date

2026-10-02

## Decision Makers

架构评审 (architecture review)

## Related

- [ADR-009](./ADR-009-gateway-workspace-isolation.md) (Gateway workspace isolation — the red-line
  source for the mobile workspace's Add-to-Chat-only rule)
- [ADR-038](./ADR-038-session-lifecycle-explicit-model.md) (explicit session lifecycle — activation
  semantics)
- [ADR-055](../zh/ADR-055-remote-runtime-node-topology.md) (the Node hosts the Runtime; mobile only
  connects to the Gateway and never touches the Node)
- [ADR-076](./ADR-076-multi-user-account-system.md) (multi-user; the **authoritative definition** of
  `can_write` — this ADR only defines how it is consumed)
- [ADR-078](../zh/ADR-078-git-status-bar.md) (the Git status bar depends on the workspace
  filesystem; mobile does not do it)
- [ADR-085](../zh/ADR-085-agent-lifecycle-state-machine.md) (the state precondition for session
  activation)
- [design/25-mobile-app.md](../../design/zh/25-mobile-app.md) (this ADR's expansion document)

---

## 1. Decision Summary

### 1.1 In one sentence

**The Mobile App adopts an IM form (a fixed four-item bottom Tab bar, a per-Tab navigation stack,
and iOS push semantics), positioned as a "portable operations terminal" for the Desktop rather than
a port of it. An agent's multiple sessions are carried on mobile by a "navigation-bar title = session
switcher + Action Sheet" (no ported session tab bar), and the backend `can_write` field from ADR-076
is consumed verbatim to implement the read-only state — write permission is never derived on the
client from `visibility`.**

### 1.2 Key decisions (detailed rationale in §4)

| # | Decision | Outcome |
|---|----------|---------|
| 1 | Product positioning | **a functional subset of the Desktop + a portable terminal** — not a port, not a remote display. The two apps are peers sharing server-side state, with independent client-side state |
| 2 | Form | **IM style** (à la WeCom/DingTalk), not a narrow-screen literal translation of the Desktop's three-column layout |
| 3 | Bottom Tabs | **fixed four items**: Chat / Projects / Docs / Settings. **Harness and Extensions do not enter mobile navigation** (the Settings page keeps greyed-out placeholder entries) |
| 4 | Navigation model | **one independent navigation stack per Tab + iOS push semantics**; switching Tabs preserves stack depth, tapping the current Tab again returns to its root |
| 5 | Secondary-page TabBar | **slides out and hides entirely**, avoiding two competing navigation layers |
| 6 | Conversation list | the Desktop's AgentList + UserList **merge into a single IM conversation stream** (two groups: Agents / Contacts, plus unread badges) |
| 7 | Conversation summary source | **the last message of the most recently active session**, not a fixed per-agent blurb |
| 8 | Multi-session carrier | **navigation-bar title = session switcher**, tapping opens an Action Sheet. **Rejected**: "one active session per agent" and "a session tab bar" |
| 9 | Session switch implementation | must go through the atomic migration equivalent to `chatStore.openSession` (UI switch + `open_session` + HTTP history reload), not a local page change |
| 10 | Read-only sessions | **do not send `open_session`** (activation is a global lifecycle change; a viewer has no right to start it and nobody can be held responsible for closing it) |
| 11 | Write-permission source | **the backend `can_write` is the sole authority.** Deriving it from the `visibility` label is forbidden; a missing `can_write` degrades to writable |
| 12 | Read-only UI | banner + hide approval cards / AskQuestion + **do not render the textarea** + **disable rather than hide** the visibility icon |
| 13 | Visibility of new sessions | defaults to **Private**, aligned with ADR-076 `create_frontend_session` |
| 14 | Workspace | **Add to Chat only, no file preview** (no Monaco; and this satisfies ADR-009 §5) |
| 15 | Kanban | on a narrow screen, **degrade to a horizontal status-Chip filter** — no drag-and-drop board |
| 16 | Local Gateway | the mobile app has **no local Gateway process** → the Desktop's start/stop/restart is disabled in v1; the address configuration is kept |
| 17 | Frontend code reuse | **reuse only the protocol-layer type definitions**; do not share the component or state layers |

### 1.3 Invariants (must hold)

1. **Single source of truth for permissions**: mobile must not produce a second write-permission
   decision. Every UI disabled because `can_write === false` must be traceable to the backend field;
   it must never be produced by `visibility`, a local cache, or UI guesswork.
2. **Public visibility ≠ writable**: a 🌐 public session can be read-only. Any "public implies you
   can type" implementation is a bug.
3. **Degradation opens up, it never locks down**: a missing field, an optimistically created
   session, an older Runtime omitting the field → always treat as **writable** and let the backend
   reject. Never freeze all controls because "the answer is unknown".
4. **A read-only viewer creates no lifecycle events**: read-only sessions must not trigger
   `open_session` / `close_session`.
5. **Multi-session is a business fact**: any "mobile keeps only one active session" design is not
   adopted.
6. **TabBar and back are mutually exclusive**: a secondary page must never present two competing
   exits (a bottom Tab bar and a back button).
7. **The workspace is reached only through Runtime HTTP**: mobile never connects to the filesystem
   directly (ADR-009 §5).

---

## 2. Context and Problems

### 2.1 Why this is not "a narrow version of the Desktop"

The Desktop App is a developer workbench: four columns showing navigation, the agent list, chat, and
the results area simultaneously. Its density assumes "one user, one room, one big screen". Mobile
usage is entirely different — **standing, walking, glancing**. Compressing four columns into 390pt
leaves ~97pt per column, producing four unreadable columns.

IM software (WeCom / DingTalk / WeChat) became the default form for mobile conversational products
not because it is "simpler" but because it matches a concrete fact: **on a mobile device people want
to "talk to someone", not "manage a pile of panels"**. Carrying that mental model over is closer to
real user intent than shrinking the Desktop.

### 2.2 The core conflict: multiple sessions vs screen width

The first draft of the interaction design proposed "one active session per agent", reasoning that
"mobile uses IM semantics, not the Desktop's parallel Session Tabs". **This was explicitly
rejected**:

- **Multi-session is a business fact.** One agent being driven by several topics at once is normal — an
  architect writing an ADR while reviewing someone else's PR while debugging a production issue. The
  Desktop carries this with parallel Session Tabs.
- **Mobile is just one of several operation terminals.** A session opened on the Desktop must be
  continuable on the phone; a session created on the phone must be visible on the Desktop (subject to
  permissions). Dropping multi-session makes the phone functionally unusable.
- **A narrow screen is a reason "a tab bar does not fit", not a reason "multi-session is not
  needed".** The first draft conflated these two.

The real design question therefore became: **what container carries N sessions on a 390pt-wide
screen?**

### 2.3 The temptation and the trap of permissions

Under multi-user deployment, three layers of session semantics are entangled:

| Dimension | Values | Decided by |
|-----------|--------|------------|
| Visibility | public / private | the owner (changeable) |
| Readability | readable / 404 | the backend (owner/admin/local all readable; other users only public) |
| Writability | writable / read-only | the backend (owner/admin/local only) |

The mobile UI has only one 🌐/🔒 icon, which naturally invites a lazy implementation: **"public →
you can type, private → you cannot"**. This is wrong — a public session is **read-only** for other
users and the icon is exactly the same. ADR-076 §Decision 4 already ships `can_write` as an
independent field precisely to eliminate this derivation; this ADR's job is to ensure mobile does
not reintroduce it.

---

## 3. Goals

1. On mobile, the information architecture becomes an **independently usable terminal**, not an
   auxiliary view of the Desktop.
2. An agent's multiple sessions are **fully usable** on mobile (list / create / switch / delete /
   visibility), sharing the same session data as the Desktop.
3. The permission semantics of multi-user public/private sessions have **zero divergence** on
   mobile; no second source of truth is produced.
4. The interaction follows the established habits of mainstream IM software, lowering the learning
   cost.
5. Deliverable in v1: the scope is small enough to complete the engineering build and integration in
   one iteration.

---

## 4. Decision

### D1: Positioned as a "portable terminal", a peer of the Desktop

The Mobile App is neither a remote display of the Desktop nor a mobile version of its UI — it is a
**second peer terminal**.

The two share server-side state (the same agents, the same sessions, the same permission model), but
their client-side state is independent — a session closed on the Desktop does not affect the phone,
and a session created on the phone is visible on the Desktop (subject to permissions).

**Rejected**: "Mobile is a screen-cast / simplified version of the Desktop". That would make every
mobile feature conditional on "whatever the Desktop has", contradicting the portable-terminal
positioning.

### D2: Four bottom Tabs; Harness/Extensions stay out of mobile

| Tab | Desktop counterpart |
|-----|---------------------|
| Chat | AgentList + UserList + ChatPanel |
| Projects | ProjectsView |
| Docs | DocsView |
| Settings | SettingsPage |

Harness and Extensions are **developer concepts**. Putting them in an ordinary user's bottom
navigation forces users to understand "why does my phone have something I don't need". The Settings
page keeps them as greyed entries labelled "desktop only", so the feature's existence and its
unavailability are visible at the same time.

### D3: One independent navigation stack per Tab + iOS push semantics

Each of the four Tabs owns a navigation stack:

```
chat:     [conversation list] → [chat detail]
projects: [project list] → [kanban] → [task detail]
docs:     [doc directory] → [doc content] → [review detail]
settings: [settings root] → [Profile|General|Appearance|Gateway]
```

| Rule | Behaviour |
|------|-----------|
| Entering a secondary page | the bottom TabBar **slides out and hides** |
| Switching Tabs | each stack's depth is preserved |
| Tapping the current Tab again | return to that Tab's root |
| Back | pop level by level; the TabBar slides back in |

**Why the TabBar hides on secondary pages**: neither iOS nor WeChat shows a bottom bar there. If it
stayed visible, users would face two competing navigation exits (the bottom Tabs and the top-right
back button) and be confused. Hiding it leaves exactly one way back.

**Why per-Tab stacks**: the IM habit is "leave a conversation and come back, still in that
conversation". A single shared stack forces the user back to the conversation list when returning
from the Projects tab to Chat.

### D4: The Chat home page is a unified conversation stream

The Desktop's AgentList and UserList are two separate panels. Mobile merges them into a **single IM
conversation stream** with two groups: Agents / Contacts.

Each row: avatar (with an online status dot), name, online status, **the most recent conversation
summary**, time, unread badge.

**The summary comes from "the last message of the most recently active session"** — a requirement of
IM semantics: users care about "where we left off", not "a one-line introduction to this agent".
Note this is coupled with the session switcher (§4 D5): the list summary and the session switcher
display the same session, and the two must be consistent.

### D5: The multi-session carrier — navigation-bar title = session switcher

This is the core decision of the ADR.

```
┌─────────────────────────────────┐
│  ←      ADR-085 生命周期状态机⌄  ⋯ │  ← the title IS the session switcher
│         gpt-5 · 24 条             │     shows session.title, not agent.name
├─────────────────────────────────┤
│              ↓ tap
├─────────────────────────────────┤
│      架构师 · 会话（8/12）         │
│  已加载最近 8 条，列表可滚动加载更多  │
│  ✓ ADR-085 生命周期状态机评审    🌐  │
│    Relay 模式降级清单        🌐 只读│
│    ADR-084 用户独立进程        🔒  │
├─────────────────────────────────┤
│        ＋ 新建会话                │
│        管理全部会话               │
└─────────────────────────────────┘
```

| Design point | Rationale |
|--------------|-----------|
| The title shows `session.title` | users identify a **topic**, not a bot; showing `agent.name` drains the title of information |
| The title has a chevron | makes it obviously tappable; otherwise users do not know other sessions exist |
| A 🌐/🔒 per row | visibility marker (§4 D7) |
| A "read-only" badge on read-only rows | states the non-writability up front instead of letting the send fail |
| ✓ marks the current session | positional sense |
| "8/12" + scrollable | aligns with `agentStore.fetchSessions(agentId, page)` pagination |
| "Manage all sessions" | jumps to the drawer's sessions section for bulk management |

**Why not another container** — see the alternatives table in §5.

Session management (list / create / switch / delete / visibility) has a full form in the drawer's
"sessions" section, aligned with the Desktop `SessionPanel`. Creation defaults to Private (§4 D8),
and deletion is offered only for writable sessions.

### D6: Switching sessions is an atomic migration, not a page change

Switching must run the full flow (aligned with the Desktop `chatStore.openSession`):

```
tap a session row
    ├─→ ① the frontend switches the current session + reloads the message history
    ├─→ ② POST /api/agents/{id}/sessions/{sid}/open (ADR-038 activation, idempotent)
    └─→ ③ in parallel, fetch the session config + state
```

**The key insight**: doing only ① is a real, existing bug pattern — the UI appears to switch sessions
but the backend is never told "this session now has a user", so the event stream is not delivered,
the state does not sync, and the owner side shows the session as closed. The Desktop collapses these
three steps into one atomic operation precisely because they must happen together.

**Read-only sessions skip ②**, following the rationale in the Desktop `chatStore.openSession`
comment: activation is a **global** per-session state, and if a viewer could activate, it would
create a resident session that "the viewer has no right to close (closing is a write authorization —
observers deliberately must not tear down the owner's session) and the owner does not know who is
holding it" — the lifecycle loses its owner. Read-only browsing also **does not need** activation:
history goes through `GET /messages` (read authorization), the event stream goes through a wildcard
MQTT subscription, and while the owner is using the session a viewer naturally receives the events.

### D7: Consume the backend `can_write`; local derivation is forbidden

```rust
// core/acowork-runtime/src/conversation.rs
pub struct SessionListView {
    pub session_id: String,
    pub visibility: Option<SessionVisibility>,
    pub can_write: bool,          // ← the sole authority
}
```

Mobile **consumes this verbatim**, with a decision rule isomorphic to the Desktop's
`isReadOnlySession`:

```ts
// Read-only holds only when can_write === false; a missing value is treated as writable.
function isReadOnly(canWrite: boolean | undefined): boolean {
  return canWrite === false;
}
```

**Why a missing value means writable**: the degradation direction must be "let the backend reject"
rather than "freeze every control". A session created optimistically that has not yet appeared in the
list, or an older Runtime that omits the field, must not leave the user facing a row of dead
buttons. This matches the Desktop's `lib/session-write-access.ts` comment exactly.

| scope \ session | public | private |
|---|---|---|
| admin | readable + writable | readable + writable |
| owner | readable + writable | readable + writable |
| other user | **readable (read-only)** | **invisible** (404) |
| local | readable + writable | readable + writable |

**"Public visibility ≠ writable" is an invariant of this ADR**: a 🌐 public session is read-only for
other users and the icon looks exactly the same.

### D8: The concrete UI treatment of the read-only state

```
┌─────────────────────────────────┐
│ 🔒 只读 · 来自 Alice 的共享会话，你不能发送消息 │  ← banner
├─────────────────────────────────┤
│           （消息列表，只读）        │
├─────────────────────────────────┤
│ 🔒 共享自 Alice · 只读            │  ← replaces the textarea
└─────────────────────────────────┘
```

| Control | When read-only | Rationale |
|---------|---------------|-----------|
| textarea | **not rendered** | a placeholder pretending to be editable is worse than an explicit disable |
| send / tools / attachments | not rendered | there is no write path |
| approval cards / AskQuestion | **hidden** | they would have the viewer decide on the owner's behalf |
| visibility icon | **disabled, not hidden** | the viewer must still know they are looking at a private session |
| change visibility / delete | menu items disabled + a reason subtitle | not "toast only after tapping" |

**Disabling rather than hiding the visibility icon** follows the existing Desktop
`SessionVisibilityToggle` rule — the component's comment states it explicitly: if a viewer cannot
even see the icon, they do not know what they are looking at, "and that switch would then display
'public' for a session nobody can read".

### D9: New sessions default to Private

Aligned with ADR-076's `create_frontend_session` — the creation path writes `Some(Private)`
alongside `user_id`, **hard-coded on disk rather than inferred on the read path**. The mobile
frontend submits private, closing the "created as public, bob can read" window.

### D10: The workspace only does Add to Chat

Mobile has no Monaco, so **no file preview**. The file tree and Add to Chat remain; file contents are
read by the agent during the conversation.

This simultaneously satisfies the Gateway workspace isolation of
[ADR-009 §5](./ADR-009-gateway-workspace-isolation.md) — mobile never connects to the filesystem
directly and goes only through the Gateway reverse proxy to Runtime HTTP.

### D11: Narrow-screen features degrade honestly rather than being faked

| Feature | Degradation |
|---------|-------------|
| Kanban | horizontal status Chip filter, **no drag-and-drop board** (drag on a narrow screen is unusable) |
| Code blocks | horizontal scroll, **no syntax highlighting** (no Monaco) |
| Tool calls | collapsed cards, collapsed by default |
| Local Gateway start/stop | **disabled** (the mobile app has no such process); the address configuration is kept |

The principle: **degrade honestly**. A mini drag-and-drop board that does not work is worse than
plainly saying "please do this on the desktop".

### D12: Do not share the Desktop frontend codebase

Mobile and Desktop share the **protocol-layer type definitions** (`SessionInfo`, the `can_write`
semantics) but **not the component or state layers**.

Their information architectures, component sets, and interaction paradigms are entirely different.
Forcing a shared codebase accumulates a large number of `isMobile` conditionals and ends up harder
to maintain than two separate codebases.

---

## 5. Rejected Alternatives

| Option | Why rejected |
|--------|--------------|
| **A. One active session per agent** | **Explicitly rejected by the user.** Multi-session is a business fact and mobile is one of several operation terminals; not supporting it makes mobile functionally unusable. A narrow screen is a reason "a tab bar does not fit", not "multi-session is not needed" (§2.2) |
| **B. Port the Desktop's `SessionTabBar`** | 390pt cannot fit N side-by-side tabs (at least 80pt each; 5 sessions overflow); and a tab bar is an "editor" mental model that does not match IM |
| **C. Sessions as a bottom horizontally-swiped pager** | Horizontal gestures collide with "swipe right to go back" — when the user swipes right in the message area, is it paging or going back? This is a gesture-semantics disaster zone. IM never carries sessions this way |
| **D. The session list as a top pull-down panel on the chat page** | The top pull-down collides with the swipe-right-back gesture in the start area; and once sessions are numerous you need a secondary page, circling back to the original problem |
| **E. Deriving writability from `visibility`** | **Dangerous.** Public is read-only for other users, the icons are identical, and the derivation necessarily produces incorrect authorization. ADR-076 ships `can_write` independently for exactly this reason (§2.3) |
| **F. Treating a missing field as read-only** | Wrong degradation direction. An optimistically created session or an older Runtime omitting the field would disable every control and the user could not send anything. The correct behaviour is "open up and let the backend reject" (D7) |
| **G. Sending `open_session` for read-only sessions** | Creates a resident session that "the viewer cannot close and the owner does not know is occupied" — the lifecycle loses its owner (ADR-076 §Decision 4, verbatim) |
| **H. Hiding the visibility icon instead of disabling it** | The viewer loses the information "I am looking at private content". The Desktop `SessionVisibilityToggle` explicitly chose "disable, not hide" |
| **I. A placeholder saying "cannot send" when read-only** | A placeholder looks editable, so users try repeatedly. Not rendering the textarea at all is far more honest |
| **J. Sharing the Desktop frontend codebase** | IA / components / interaction paradigms differ entirely; sharing accumulates many conditionals (D12) |
| **K. Local Gateway management on mobile** | The process does not exist on mobile. Start/stop control would be a pure pseudo-feature |
| **L. A full feature port on mobile (incl. DevMode / Git / agent creation)** | v1 scope runs away, and these depend on the local filesystem / process control / a large screen, which conflicts with the portable-terminal positioning |

---

## 6. Consequences

### 6.1 Positive

- Mobile becomes an independently usable terminal with full multi-session capability.
- The permission semantics have zero divergence from the backend, eliminating the class of wrong
  authorization that derives writability from visibility.
- The interaction follows mainstream IM habits, so the learning cost is low.
- The v1 scope is clear (4 Tabs, no debugging, no admin operations) and deliverable in one iteration.
- Every read-only UI decision has a Desktop precedent (`SessionVisibilityToggle` /
  `session-write-access.ts`) rather than being invented from scratch.

### 6.2 Negative / costs

- **Protocol-layer types must be shared across repositories**: mobile needs `SessionInfo` and the
  `can_write` semantics, so a shared type crate or package must be extracted or the definitions will
  drift.
- **Two state layers**: Desktop and Mobile each have a store, and the multi-session logic must be
  implemented twice (`fetchSessions` pagination, the `openSession` atomic migration, the `can_write`
  decision). This is the direct cost of D12.
- **Components must be rebuilt**: inset lists, Switch, Segmented, Stepper, and Action Sheet have no
  Desktop counterpart; a mobile component set must be created.
- **A larger gesture-conflict surface**: left-swipe drawer / right-swipe back / vertical scroll /
  horizontal sub-lists coexist, and this was the area where the prototype surfaced the most bugs.
- The information density of multi-session UI on a narrow screen needs continuous tuning (currently
  8 rows per screen).

### 6.3 Boundaries / exceptions

- **User-to-user chat has no session concept**: it is stored on the Gateway side under a normalized
  pair id (`min__max`), is 1:1, and **does not enter the session switcher**. Only agent conversations
  are multi-session.
- **local mode**: `can_write` is always true, all permission-related UI is simply hidden, and the
  logic is unchanged.
- **An agent with only one session**: the switcher is still shown (tapping opens a list with one entry
  plus "new") and is not special-cased, keeping the interaction predictable.
- **An agent with no sessions** (e.g. a stopped doc admin): shows "no sessions yet" and keeps the
  create entry point.

### 6.4 Rollback

This ADR is entirely client-side decisions and changes no protocol, backend, or data structure.
Rollback = roll back the mobile app version, with zero impact on the Desktop. This is deliberate:
confining the information-architecture decision to the client guarantees that getting it wrong
**produces no migration that needs cleaning up**.

### 6.5 Verification record

The interaction prototype `docs/prototypes/mobile-im-v1.html` passed two-layer automated
verification:

| Layer | Tool | Cases | Result |
|-------|------|-------|--------|
| Behaviour | jsdom (real clicks + synthetic touch events) | 60 | 60 passed |
| Layout | WKWebView real-device `getBoundingClientRect` | 44 | 44 passed |

The layout layer is not omittable: jsdom returns 0 for every geometric value and would mask real
overflow. The real-device run exposed 4 genuine defects (the navigation-bar subtitle never rendered
due to a DOM hierarchy error, the session switcher broke the navigation bar's grid column, the
session list never scrolled because `max-height:46vh` was too large, and session names were rendered
twice in the Action Sheet) — all purely client-side, consistent with the rollback expectations of
§6.4.

### 6.6 Known technical debt / follow-ups

- The multi-session logic is implemented twice on mobile and Desktop. Consider extracting a
  **pure-function shared package** above the protocol layer (`isReadOnlySession` / session sorting /
  summary extraction) without forcing a shared state layer.
- Mobile App Extensions (iOS) not evaluated.
- Collaborative document editing (ADR-079 Yjs) on mobile not evaluated — expected after v2.
- Mobile push notifications (notify when an agent finishes) are out of v1 scope.

## 7. Implementation Record

(to be added once the engineering build starts)
