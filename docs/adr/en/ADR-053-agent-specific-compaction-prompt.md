# ADR-053: Agent-Level Compaction Prompt — prompts/summary.md Replaces the Unified COMPACTION_SYSTEM_PROMPT

> **Chinese source of truth**: [ADR-053](../zh/ADR-053-agent-specific-compaction-prompt.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Decided

## Date

2026-08-21

## Decision Makers

大鱼 (Dayu)

## Related

- [ADR-011](./ADR-011-compaction-as-distillation.md) — unified summary and distillation strategy
- [ADR-014](./ADR-014-loop-module-decomposition.md) — Loop module decomposition

---

## 1. Decision summary

The system prompt has long been **per-agent** (declared as `prompts/*.md` inside the
`.agent` package), but the compaction and distillation prompt is still a **globally
unified** hardcoded constant `COMPACTION_SYSTEM_PROMPT` (`core/acowork-runtime/src/prompt.rs`).
Agent types differ enormously in what must survive a summary: a software engineer
needs file paths, function names, and technical decisions; a support agent needs user
intent and resolution; a documentation agent needs structure and references. One unified
prompt cannot express that.

This ADR aligns the compaction prompt with the existing system prompt pattern: **each
agent may declare its own `prompts/summary.md`**, falling back to the built-in
`COMPACTION_SYSTEM_PROMPT` when absent.

**Core decisions**

1. **Package-level declaration** — `prompts/summary.md` is the agent-specific
   compaction/distillation system prompt, at the same level and by the same mechanism as
   `system.md`.
2. **Priority chain** (high to low):
   `AgentCore.compaction_prompt` (from the package) > `COMPACTION_SYSTEM_PROMPT` (built-in
   fallback).
3. **No semantic mixing** — the compaction path MUST NOT borrow
   `system_prompt_override` (the `agent_config.json` field that overrides the main
   conversation system prompt). Compaction is an independent summarization task, so its
   instructions belong to the package, not to runtime config.
4. **Excluded from the main prompt** — `prompt_builder` MUST skip `summary.md` when
   assembling the main system prompt, so summary meta-instructions never leak into every
   LLM call.
5. **Uniform across all paths** — the main compaction path (`loop_context.rs`) and the
   distillation paths (`compact_messages` in `episode_distill.rs`, plus the reserved
   `distill_on_session_end`) all use the same resolved value. Loading is centralized in
   Phase A, so Gateway and Standalone modes behave identically.

## 2. Background

**2.1 The unified prompt cannot serve everyone.** `COMPACTION_SYSTEM_PROMPT`
(`prompt.rs:18`, ~850 characters) defines the output shape (`<summary>` plus
`<user_intent>`), the language rules, and hard constraints. It works for generic
summaries but cannot encode domain requirements:

- a software engineer agent must keep paths like `core/acowork-runtime/src/...` and
  function names, or the next session cannot resume the work;
- a project management agent must keep decision owners, deadlines, and risks;
- a documentation agent must keep document structure and reference relations.

The **package author** knows these requirements best, so they belong in the `.agent`
package rather than hardcoded in the Runtime. (The historical `<triples>` / `<entities>` blocks
were withdrawn in the M3 rework per ADR-057 §0.2 and are retained here only as a decision
record.)

**2.2 `system_prompt_override` mixed two orthogonal concerns.** The compaction path used
to read `self.core.system_prompt_override` and fall back to
`COMPACTION_SYSTEM_PROMPT`. But that field is the runtime override for the **main
conversation** system prompt (`agent_config.rs:158`; `session_init.rs` documents `None` as
meaning "use the compiled manifest prompt"). Borrowing it coupled "main conversation
override" with the "compaction instructions":

- setting `system_prompt_override` to change the conversation identity also silently changed
  compaction behaviour;
- customizing compaction rules required going through the wrong entry point.

**2.3 Why now** — the project is not live, so there is no compatibility burden and the
optimal design can be applied directly. The `prompts/*.md` mechanism is already stable,
making `summary.md` a zero-cost extension. Fixing distillation at the same time avoids the
split where compaction uses custom rules while distillation still uses the default.

## 3. Design

**3.1 File convention**

```
examples/senior-engineer-agent/
├── prompts/
│   ├── system.md        # main conversation identity → main system prompt
│   ├── constraints.md   # behavioural constraints → main system prompt
│   └── summary.md       # compaction/distillation instructions (NEW)
├── skills/
└── manifest.toml
```

**3.2 Load and resolution chain**

```rust
// prompt_builder.rs
pub const COMPACTION_PROMPT_FILE: &str = "summary.md";

/// Reads prompts/summary.md; returns None when missing or blank
pub fn load_compaction_prompt(package_dir: &Path) -> Option<String>;
```

- `AgentCore` gains a `compaction_prompt: Option<String>` field, parallel to but
  semantically independent from `system_prompt_override`.
- **Loading is centralized in Phase A** (`agent_init.rs` step 3, after the system prompt is
  built): `load_compaction_prompt(&loaded.package_dir)` runs once and the result is stored
  in `AgentBootContext.compaction_prompt`. Both AgentCore construction paths inject from
  that field — Gateway mode (Phase B in `session_init.rs` via `Arc::get_mut`) and Standalone
  mode (direct construction in `cli.rs`) — so the same `.agent` package resolves the
  same instructions in both modes, with no mode split.
- `loop_context.rs` resolves
  `self.core.compaction_prompt.as_deref().unwrap_or(COMPACTION_SYSTEM_PROMPT)`.
- The three distillation entry points (`compact_full_context` / `compact_messages` /
  `distill_on_session_end`) gain a `compaction_prompt: Option<&str>` parameter supplied by
  the caller from `AgentCore`.

**3.3 Exclusion from the main prompt**

`build_system_prompt_with_mode` skips the file by **exact name** (`summary.md`) while
iterating `prompts/*.md`. This is the only exception to the "concatenate everything in
`prompts/`" rule, because the instructions are meta-directives for the summarizing LLM
and would otherwise pollute every LLM call.

Exclusion and loading use the **same match criterion** (the exact name `summary.md`), so
"excluded from the main prompt" and "loaded as the compaction prompt" always point at the
same file. Any other name (`SUMMARY.md`, `summary.txt`) is treated as an ordinary prompt
section rather than silently ignored.

**3.4 Unchanged**

- The built-in `COMPACTION_SYSTEM_PROMPT` remains the fallback, so packages without
  `summary.md` behave exactly as before.
- `build_compaction_system_prompt(base, identity_context)` still appends the user identity
  (language) instructions after the base — `summary.md` as the base benefits from the same
  language rules.
- `COMPACT_PROMPT` (the user message template carrying the `<conversation>` body) is
  unchanged.

**3.5 Limits and reservations**

- **Loaded statically at startup** — `compaction_prompt` is read once in Phase A and is not
  reloaded on package upgrade or hot reload, matching the system prompt behaviour. A package
  author updating `summary.md` must restart the Runtime.
- **`distill_on_session_end` is a reserved path** — it currently has no caller. Its signature
  already takes `compaction_prompt` but is not activated. The live distillation paths are tail
  distillation (`compact_messages` in `loop_session.rs`) and the main compaction path
  (`compact_via_llm` in `loop_context.rs`); activating session-close distillation later must pass
  the same field from `AgentCore`.

## 4. Impact

| File | Change |
|------|--------|
| `package/prompt_builder.rs` | `load_compaction_prompt` (distinguishing missing from read failure) + exclude `summary.md` from the main prompt by exact name + 7 unit tests |
| `agent/agent_core.rs` | new `compaction_prompt` field + init + `Clone` impl |
| `startup/agent_init.rs` | Phase A loads `compaction_prompt` into `AgentBootContext` |
| `startup/context.rs` | `AgentBootContext` gains `compaction_prompt: Option<String>` |
| `startup/session_init.rs` | Phase B injects from ctx instead of reading the file |
| `cli.rs` | Standalone branch injects from ctx, removing the mode split |
| `agent/loop_context.rs` | compaction resolution uses `compaction_prompt`; drop the `system_prompt_override` borrow |
| `episode_distill.rs` | three entry points gain `compaction_prompt: Option<&str>` |
| `agent/loop_session.rs` | tail distillation passes `core.compaction_prompt` |
| `examples/senior-engineer-agent/prompts/summary.md` | example: engineering compaction rules (keep file paths, technical decisions, verification evidence) |

**Behaviour changes**

| Scenario | Before | After |
|----------|-------|-------|
| Package has no `summary.md` | built-in default | built-in default (unchanged) |
| Package has `summary.md` | (no such concept) | compaction and distillation use the package instructions |
| User sets `system_prompt_override` | changes both the main prompt and compaction | changes only the main prompt (semantics clarified) |

**Verification**

- `cargo build -p acowork-runtime` — passes.
- `cargo test -p acowork-runtime --lib` — 888 passed, including the 7 new `prompt_builder` tests.
  (`test_run_falls_back_to_user_message_when_raw_is_none` is a pre-existing timing-related
  flake, unrelated to this change.)
- `cargo clippy -p acowork-runtime --all-targets -- -D warnings` — passes with zero warnings.
- Integration tests `mqtt_e2e_full` / `mqtt_e2e` / `conversation_session_tokens` /
  `builtin_tools_mutation` all green; `shell_risk_e2e` failure is pre-existing and unrelated.

## 5. Alternatives

**5.1 Keep `system_prompt_override` as the compaction override entry (rejected).** More
flexible on paper, but it conflates "main conversation override" with "compaction
instructions"; runtime config is user tuning while a package declaration is author intent, and
the two should stay independent.

**5.2 Put `summary.md` in the package root (rejected).** This would avoid the
`prompt_builder` exclusion, but it breaks the principle that all prompt files live under
`prompts/` and is asymmetric with `system.md`. One line of exact-name matching is a fair
price for organizational consistency.

**5.3 Use frontmatter metadata for the instructions (rejected).** Adding YAML frontmatter
parsing for a single file is over-engineering; the package has no such mechanism today and the
filename convention already expresses the intent.
