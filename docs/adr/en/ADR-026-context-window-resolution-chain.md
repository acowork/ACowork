# ADR-026: Context Window Resolution Chain (per-agent context window cap)

> **Chinese source of truth**: [ADR-026](../zh/ADR-026-context-window-resolution-chain.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Accepted (revised by ADR-074 on 2026-09-15; where the two conflict, **ADR-074 governs**)

## Date

2026-07-05

## Decision Makers

大鱼 (Dayu)

## Blast radius

- `core/acowork-core/src/manifest.rs` — `LlmConfig` gains a `context_window` field
- `core/acowork-runtime/src/config.rs` — new `DEFAULT_CONTEXT_WINDOW` constant
- `core/acowork-runtime/src/agent_config.rs` — `AgentConfig` gains `context_window` + first-start seed logic
- `core/acowork-runtime/src/agent/agent_core.rs` — new `context_window_override` + `manifest_context_window` fields; `context_trim_budget` reworked
- `core/acowork-runtime/src/cli.rs` — manifest → AgentCore seeding logic
- `core/acowork-runtime/src/agent/session/session_manager.rs` — propagate `context_window` on session creation
- `core/acowork-gateway/src/http/agent_config.rs` — `AgentConfigResponse` gains `context_window` / `context_window_source` / `manifest_context_window`
- `apps/acowork-desktop/src/lib/types.ts`, `apps/acowork-desktop/src/stores/chatStore.ts` — frontend state
- Agent Setup panel (the context window input)

---

> **⚠️ Part of this document's semantics were revised by [ADR-074](ADR-074-per-session-context-window-override.md) on 2026-09-15**
>
> - **`0 = unlimited` is abolished**: `0` / `null` / a missing field are all uniformly "unset (invalid)", and the resolution chain skips that layer. The chain's end `DEFAULT_CONTEXT_WINDOW = 200_000` is the backstop and **is also the de-facto ceiling** (ADR-074 §1.2 D1, §1.3, §6).
> - **The chain gains a highest-priority Layer 0: per-session `context_window`** (via the ADR-047 session-config pipeline). This document's "Cancelled feature: per-session context window override" section is void (ADR-074 §3.1).
> - **The effective range** becomes `FLOOR..=CEILING` (`FLOOR = 8_192`, `CEILING = 4_194_304`); an out-of-range value returns 400 from HTTP `PUT /sessions/{sid}/config` (ADR-074 §3.3).
> - **Resolution ownership moved**: resolution no longer lives in `AgentCore` (a per-agent template under clone-on-write, so writing a session value leaks across sessions), becoming a stateless pure function `resolve_effective_context_window` + `AgentCore::context_trim_budget_with(resolved_cap, model)` (ADR-074 §1.5, §3.2).
> - The body below is preserved as-is to retain the decision history; **where it conflicts with ADR-074, ADR-074 governs**.

## Context

`ACowork`'s context window budget is computed by `AgentCore::context_trim_budget()`:

```rust
pub fn context_trim_budget(&self, model_name: &str) -> u64 {
    self.get_model_capabilities(model_name)
        .map(|caps| caps.effective_input_budget(max_output_limit))
        .unwrap_or_else(|| self.config.history_max_tokens)  // fallback: 128K
}
```

**The problems**:

1. **The user cannot limit the context window size.** The budget is entirely determined by the model capability (`ModelCapabilitiesInfo.context_window`); the user cannot say "even though this is claude-3.5-sonnet (200K), I only want to use 128K". This matters in cost-sensitive scenarios (reducing token consumption) and in debugging scenarios (reproducing small-window behavior).
2. **No package author preset.** `manifest.toml` has no `context_window` field, so a package author cannot recommend an appropriate context window cap for an agent.
3. **No provenance for the context window source.** Analogous to the temperature traceability requirement in ADR-025, the user cannot see where the currently effective cap comes from (user setting / package author preset / system default vs the model limit).
4. **`history_max_tokens` is global system config.** The hardcoded 128K fallback treats every agent identically and cannot be tuned per agent.

**Design goal**: following the ADR-025 temperature resolution chain pattern, introduce a
**per-agent three-tier fallback chain** for the context window size, taking the minimum
with the model's own context window at actual use time.

## Design

### The context window resolution chain (3 layers)

```text
Layer 1 (highest priority)  agent_config.json.context_window      the user's Agent-level setting
    ↓ if None or 0
Layer 2                     manifest.llm.context_window            the package author default
    ↓ if None or 0
Layer 3 (final fallback)    DEFAULT_CONTEXT_WINDOW = 200_000    hardcoded (200K tokens)
```

- **Range**: `0` – `1_000_000` tokens (0 = unlimited, decided by the model itself) — **revised by ADR-074**: `0` means unset (invalid) and falls through to the next layer; the range becomes `FLOOR..=CEILING` (ADR-074 §1.2 D6, §3.3)
- **Unit**: tokens, consistent with `ModelCapabilitiesInfo.context_window`
- **Default**: `200_000` (200K tokens), which covers mainstream model context windows (GPT-4o 128K, Claude Sonnet 200K, DeepSeek-V3 128K)

### Actual effective logic: take the min with the model capability

After the chain produces `resolved_cap` (the user's intended cap), take the minimum with the
model's `context_window`:

```python
resolved_cap = agent_config.context_window or manifest.llm.context_window or DEFAULT_CONTEXT_WINDOW
# revised by ADR-074: there is no longer a "0 = unlimited"; 0 / None / out-of-range are
# all invalid and fall through to the next layer
model_budget = caps.effective_input_budget(max_output_limit)
effective_budget = min(resolved_cap, model_budget)
```

| User setting | manifest | Model context_window | effective_budget |
|---|---|---|---|
| None | None | 128K | 200K → min(200K, 128K - reserve) ≈ 96K |
| None | 64K | 128K | 64K → min(64K, 128K - reserve) ≈ 64K - reserve |
| 300K | - | 128K | 300K → min(300K, 128K - reserve) ≈ 128K - reserve |
| 100K | 200K | 1M | 100K → min(100K, 1M - reserve) ≈ 100K - reserve |
| 0 | 0 | 128K | ~~unlimited~~ **abolished**: 0 = unset → both layers skipped → the 200K backstop → min(200K, 128K - reserve) ≈ 128K - reserve (ADR-074 §6) |

### New `AgentCore` fields

```rust
/// Per-agent context window cap (from agent_config.json, set via the Agent Setup panel).
/// Layer 1 in the resolution chain. 0 means "no limit".
pub(crate) context_window_override: Option<u64>,

/// Context window cap from manifest.toml [llm].context_window (Layer 2).
/// Seeded at agent startup in cli.rs; independent of context_window_override
/// so the resolution chain is self-contained in AgentCore.
pub(crate) manifest_context_window: Option<u64>,
```

**Design rationale** (aligned with `temperature_override` / `manifest_temperature`):
encapsulation (the resolution logic is entirely held by `AgentCore`), testability (the chain
can be tested without loading a manifest), and consistency (identical usage pattern to the
temperature fields).

### The reworked `context_trim_budget`

```rust
/// Resolve the effective context window budget for history trimming.
///
/// NOTE (ADR-074): "0 = no cap" is superseded — 0 means unset/invalid and falls
/// through to DEFAULT_CONTEXT_WINDOW (200K), which is also the de-facto ceiling.
/// The per-session Layer 0 and the session-ownership constraint move this logic
/// into a stateless `resolve_effective_context_window`; AgentCore only receives the
/// resolved value via `context_trim_budget_with(resolved_cap, model)` (ADR-074 §1.5, §3.2).
/// Resolution chain for the user-configured cap:
///   1. agent_config.json.context_window (Layer 1)
///   2. manifest.llm.context_window (Layer 2)
///   3. DEFAULT_CONTEXT_WINDOW (Layer 3, 200K)
///
/// The resolved cap is then clamped to the model's actual context window:
///   effective = min(resolved_cap, model.effective_input_budget)
///
/// When resolved_cap == 0, no cap is applied (use model's full capacity).
pub fn context_trim_budget(&self, model_name: &str) -> u64 {
    let max_output_limit = self.max_output_tokens_limit_for_model(model_name);
    let resolved_cap = self
        .context_window_override
        .or(self.manifest_context_window)
        .unwrap_or(DEFAULT_CONTEXT_WINDOW);

    self.get_model_capabilities(model_name)
        .map(|caps| {
            let model_budget = caps.effective_input_budget(max_output_limit);
            if resolved_cap == 0 {
                model_budget
            } else {
                std::cmp::min(resolved_cap, model_budget)
            }
        })
        .unwrap_or_else(|| {
            // No model capabilities — fall back to the resolved cap directly
            if resolved_cap == 0 {
                self.config.history_max_tokens
            } else {
                std::cmp::min(resolved_cap, self.config.history_max_tokens)
            }
        })
}
```

### New `manifest` `[llm]` field

```toml
[llm]
# Per-agent context window size limit in tokens.
# Resolution chain at runtime:
#   agent_config.json → this manifest value → DEFAULT_CONTEXT_WINDOW (200K)
# When absent (None), falls through to the next level.
context_window = 200000  # optional
```

```rust
// manifest.rs — LlmConfig
#[serde(default, skip_serializing_if = "Option::is_none")]
pub context_window: Option<u64>,
```

### New `AgentConfig` field

```rust
// agent_config.rs — AgentConfig
/// Resolution chain at runtime (Layer 1 = highest priority):
/// 1. **this field** — the user's agent-level setting (set via the Agent Setup panel)
/// 2. `manifest.llm.context_window` — the package author default
/// 3. `DEFAULT_CONTEXT_WINDOW` — the hardcoded final fallback (200K)
///
/// `None` means "I don't have an opinion" — fall through to the next level.
/// `Some(0)` means "no limit" — use the model's full context window.
/// The user can clear this value in the UI to revert to the manifest default.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub context_window: Option<u64>,
```

### Context window source tracking: `AgentConfigResponse`

The Gateway `AgentConfigResponse` gains:

```rust
/// Effective context window cap (tokens). Resolved from the per-agent chain:
///   agent_config.json → manifest.llm.context_window → DEFAULT_CONTEXT_WINDOW (200K)
pub context_window: Option<u64>,

/// Source of the effective context window value:
/// - "config"    — from agent_config.json (the user's Agent Setup panel setting)
/// - "manifest"  — from manifest.toml [llm].context_window (the package author default)
/// - "default"   — from DEFAULT_CONTEXT_WINDOW (hardcoded 200K)
pub context_window_source: Option<String>,

/// The manifest-level context window cap — for frontend placeholder display
/// e.g. "leave empty to use the package default 200K"
pub manifest_context_window: Option<u64>,
```

The determination is `config` if `config.context_window is not None`, else `manifest` if
`manifest_context_window is not None`, else `default`.

### Constant

```rust
// acowork-runtime/src/config.rs
/// Default context window cap for the per-agent resolution chain.
/// 200K tokens covers the majority of current flagship models
/// (GPT-4o 128K, Claude Sonnet 200K, DeepSeek-V3 128K).
/// **Keep aligned** with `acowork_gateway::http::agent_config::DEFAULT_CONTEXT_WINDOW`.
pub const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;
```

## Implementation plan

**Phase 1 — manifest + config data structures.** `LlmConfig` gains
`context_window: Option<u64>`; `config.rs` gains `DEFAULT_CONTEXT_WINDOW: u64 = 200_000`;
`AgentConfig` gains `context_window: Option<u64>`. All low risk (purely additive optional
fields).

> **ADR-074 revision**: Phase 2.1 / 2.3 below, and Phase 3.2's "the resolution chain lives
> inside `AgentCore::context_trim_budget` so call sites need no change", are superseded by
> [ADR-074 §1.5](ADR-074-per-session-context-window-override.md) — `AgentCore` must not hold
> session-level state (the per-agent template under clone-on-write would leak across
> sessions), so resolution becomes a stateless pure function + pure parameter injection
> through `context_trim_budget_with(resolved_cap, model)`.

**Phase 2 — `AgentCore` restructuring.** Add the `context_window_override` +
`manifest_context_window` fields, seed `manifest_context_window` in `new_with_observer`,
propagate the new fields in `clone_shallow`; seed `core.manifest_context_window =
manifest.llm.context_window` in `cli.rs`; rework `context_trim_budget()` with the chain plus
the min logic. Low risk for the field additions (following the temperature pattern), medium
for the core behavior change.

**Phase 3 — session propagation + injection point adaptation.** `session_manager.rs:626`
calls the reworked `context_trim_budget` on session creation (call sites need no change
because the rework happens inside the method); all 5 `context_trim_budget` call sites in
`loop_context.rs` need no change, only the explanatory comment; `agent_config.rs` gets the
first-start seed `seeded.context_window = manifest.llm.context_window` (analogous to the
temperature seed).

**Phase 4 — Gateway + frontend.** `AgentConfigResponse` gains the three fields;
`agents.rs` populates them (like `temperature_source` / `manifest_temperature`); the
frontend `types.ts` and `chatStore.ts` gain the state fields; the Agent Setup panel gains a
context window number input (in tokens) with a placeholder and a source display.

**Phase 5 — build verification**: `cargo build && cargo clippy --all-targets -- -D warnings &&
cargo test` plus `npx tsc --noEmit` in the desktop app.

## Alternatives

### A — manifest field only, no `agent_config.json` layer

Set `context_window` directly in `manifest.toml`, changing neither `agent_config.json` nor
the UI. Smallest change, but the user cannot adjust it in the Agent Setup panel and the
package author must anticipate every usage scenario. **Rejected**.

### B — wrap outside `context_trim_budget`

Do not store fields in `AgentCore`; instead do the min operation at each of the 6 call sites
(5 in `loop_context.rs` + 1 in `session_manager.rs`). This avoids changing the `AgentCore`
structure, but duplicates the resolution logic across 6 call sites where it is easy to miss
or diverge from, and disperses the chain logic, which hurts unit testing. **Rejected**.

### C (chosen) — `AgentCore` field storage + an internal rework of `context_trim_budget`

All 6 call sites benefit automatically with no per-site change; the chain logic is
concentrated in `AgentCore` and is testable; consistent with the ADR-025 temperature
`AgentCore` field pattern.

## Cancelled feature: per-session context window override (voided by ADR-074)

Consistent with ADR-025, this proposal **excludes** a per-session context window override.
All sessions share one agent-level context window cap.

> **Void**: the feature was subsequently established and adopted by
> [ADR-074](ADR-074-per-session-context-window-override.md) as the highest-priority
> Layer 0, read and written through the ADR-047 session-config pipeline (**not** cached in
> `SessionState`, and `AgentCore` must not hold the session value either). Of the 4 original
> suggestions, the 1st (a new `SessionState` field) and the 3rd (`context_trim_budget` reading
> the session) were explicitly rejected by ADR-074 §1.5, while the 4th (a 4-layer chain) is
> consistent with ADR-074 §3.1.

If this feature is added in the future (a user temporarily adjusting the cap mid-session), it
should be a separate proposal covering: a `context_window` field on `SessionState`; a
context window slider in the frontend session toolbar; `context_trim_budget` reading the
session-level setting first; and the chain extended to 4 layers (per-session → agent_config.json
→ manifest → default).

## Risks and mitigations

| Risk | Probability | Impact | Mitigation |
|---|---|---|---|
| The 200K default is meaningless for small models (e.g. 32K) | medium | low | the min operation truncates to the model capability automatically, so it cannot be wrong |
| The user mistakenly sets 0 (thinking it means "minimum") | low | low | **resolved by ADR-074 §6**: the 0 semantic changes to "unset/invalid" and the chain simply falls through; in the UI 0 is equivalent to "clear the override" and the anomalous "unlimited" state no longer exists |
| The resolution chain lives inside `AgentCore` (Phase 2.3) | — | medium | **revised by ADR-074 §1.5**: `AgentCore` is a per-agent template (clone-on-write), so a session-level value written there would leak across sessions → a stateless pure function + pure parameter injection |
| Semantic overlap with `history_max_tokens` | low | low | `history_max_tokens` is retained as the final fallback when there is no model capability; the per-agent `context_window` is the user's intended cap; the two are complementary |
| `context_trim_budget` becomes more complex | low | low | the logic increment is small (+~10 lines) and keeps the early-return pattern |
