<h1 align="center">ACowork.AI — Collaborate with your AI Colleagues</h1>

<p align="center">
  <img src="assets/brand-mark.svg" alt="ACowork" width="360">
</p>

<p align="center">
  🏗️ <strong>Declarative Agent Platform · Decentralized · High-Security · Scalable</strong><br>
  ⚡️ <strong>Easy to build an agent colleague.</strong><br>
  ⚡️ <strong>Easy to share an agent colleague.</strong><br>
  ⚡️ <strong>Easy to deploy agent colleagues.</strong>
</p>

<p align="center">
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License" /></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/language-Rust-ff6600" alt="Language" /></a>
  <a href="./docs/design/zh/"><img src="https://img.shields.io/badge/docs-design-brightgreen" alt="Docs" /></a>
  <a href="./apps/acowork-desktop/"><img src="https://img.shields.io/badge/status-alpha-orange" alt="Status" /></a>
</p>

<p align="center">
  <a href="README.zh.md">简体中文</a>
</p>

---

<p align="center">
  <table>
    <tr>
      <td width="50%" align="center" valign="top">
        <img src="./assets/1.jpg" alt="Multi-Agent Collaboration &amp; Memory" width="100%" />
        <br />
        <em>Collaborate with multiple AI colleagues — each with private memory, real-time context awareness, and tool execution.</em>
      </td>
      <td width="50%" align="center" valign="top">
        <img src="./assets/2.jpg" alt="Debug Panel &amp; Context Snapshots" width="100%" />
        <br />
        <em>Full-stack development framework with iterative debugging, token tracking, and context snapshots for deep insight into AI reasoning.</em>
      </td>
    </tr>
  </table>
</p>

---

## What is ACowork.AI?

ACowork.AI is a **multi-user, distributed, AI-native collaboration platform**. It brings together people, AI
colleagues, projects, and documents into one runtime — so a team can plan work, write specs, review code, and ship
features alongside their agents the same way they would alongside a human teammate.

- **Multi-user by default.** Real accounts, roles, sessions, and per-user audit trails. No more single-actor demo mode.
- **Distributed by default.** Gateway is the single control plane; Node Agents run on every machine that hosts
  Runtimes — your GPU box, your laptop, your cloud VM — and the Gateway talks to all of them over the same protocol.
- **AI-native project & document management.** Project / task management (`acowork-pm`) and the online document
  library (`acowork-doc`) are first-class services — agents read, write, and collaborate on them through REST +
  MCP, with real-time multi-user editing.
- **Seamless human ↔ AI collaboration.** Users talk to agents in the same chat surface where humans talk to each
  other; agents subscribe to project / document events; intents route between any two actors.
- **Standalone or cluster.** Run a single host for personal use; flip a flag and the same binary becomes a
  cluster spanning every machine in your team — one protocol, no code branches.

Every Agent is still an independent **"digital being"**: its own runtime process, private memory, workspace, and
configuration. **Tune prompt, tools, and memory = build an AI colleague.** Personal and sensitive data is
automatically stripped during packaging, so you can share an agent's capabilities freely without leaking your
private memories.

---

## 🎯 Why ACowork

These are the bets ACowork is built around — and the reasons to choose it over a single-process agent playground.

| | |
|---|---|
| 👥 **Multi-user, not single-actor** | Real accounts, roles, sessions, audit. The same chat / project / document surface is shared by humans and AI colleagues. |
| 🌐 **Distributed agents, one control plane** | Gateway brokers MQTT + HTTP reverse proxy; Node Agents run on every host. A GPU box, a workstation, and a cloud VM all look the same to the Gateway. |
| 🗂️ **AI-native project & document management** | `acowork-pm` and `acowork-doc` are first-class services with REST + MCP. Agents read, write, and edit them like any other teammate — including real-time multi-user editing (Yjs CRDT). |
| 🤝 **Humans and AI in one conversation** | One chat surface, one project tree, one doc library. AI subscribes to events, raises intents, and ships work alongside humans — no "AI side panel" silo. |
| 🚀 **Standalone or cluster — same binary** | Run a single host for personal use, or scale out to a cluster across your team. One protocol path, no local-vs-remote code branches. |

---

## ✨ Highlights

| | |
|---|---|
| 👥 **Multi-user, not single-actor** | Real accounts, roles, sessions, per-user audit. Humans and AI colleagues share the same chat / project / doc surface. |
| 🌐 **Distributed agents, one control plane** | Gateway brokers MQTT + HTTP reverse proxy; Node Agents run on every host that hosts Runtimes (GPU box / workstation / cloud VM) — single-host and multi-host share one protocol path. |
| 🗂️ **AI-native project & document management** | `acowork-pm` and `acowork-doc` are first-class services; agents read, write, and collaborate on tasks and documents through REST + MCP — including real-time multi-user editing (Yjs CRDT). |
| 🤝 **Human ↔ AI in one conversation** | One chat surface, one project tree, one doc library. AI subscribes to project / doc events and ships work alongside humans — no "AI side panel" silo. |
| 🚀 **Standalone or cluster — same binary** | Start a single host for personal use; flip a flag and the same binary becomes a cluster across your team. No local-vs-remote code branches in Gateway. |
| 🧩 **Declarative agents** | `.agent` packages contain manifest + prompts + skills — **no executable code**, signed and verified at install time. |
| ⚙️ **Universal runtime** | A single Rust binary loads any `.agent` package; Agents connect directly to LLM APIs — no Gateway proxy, no extra latency. |
| 🔒 **Process-level isolation** | Every Agent runs as an independent OS process with its own filesystem, Grafeo DB, and sandboxed tool execution. |
| 🧠 **Biomimetic memory** | 3-tier / 5-class layered memory on a per-Agent Grafeo graph DB — HNSW + BM25 hybrid retrieval, associative diffusion. |
| 🛡️ **Three-layer security** | Package signing + OS process sandbox + Wasmtime tool sandbox. |
| 💬 **Intent collaboration** | Agents advertise capabilities in a registry, route requests/observations, sync or async, with the Gateway as broker. |
| 🛠️ **Full-stack dev loop** | Desktop App (Tauri v2) supports DevMode: conversational debug, skill hot-reload, breakpoints, recording & replay, publishing wizard. |

---

## 🏛️ Architecture

ACowork has a flat three-tier topology: **Gateway** (one control plane per cluster), **Node Agent** (one per
machine that hosts Runtimes), **Runtime** (one per `.agent` instance). Every actor — human, AI agent, project
service, document service — talks to the Gateway over the same protocol path.

| Tier | Component | Role |
|------|-----------|------|
| **Gateway** (`acowork-gateway`) | One keep-alive Rust process per cluster | HTTP API on `:19876`, embedded MQTT broker on `:19875`, reverse proxy to every Node / Runtime, package manager, intent router, global resources (LLM providers / MCP / budget / cron / rate-limit). |
| **Node Agent** (`acowork-node`) | One per machine that hosts Runtimes | Process table (spawn / kill / reap Runtimes), local package store, MQTT enrollment with the Gateway, local reverse proxy on `:19900`, LSP sidecar supervisor, node-local fs browse. |
| **Runtime** (`acowork-runtime`) | One per running `.agent` instance | Universal Rust binary that loads a `.agent` package, hosts the LLM loop, Grafeo memory, tools, and per-agent HTTP on loopback. |

Alongside these, four co-resident services plug into the same Gateway protocol path and are reachable to Runtimes
through the same REST + MCP surface that humans use:

| Service | Role |
|---------|------|
| `acowork-user` | User domain — accounts, credentials, roles, profiles / avatars, user↔user chat. |
| `acowork-pm`   | Project & task management — first-class service, agents and humans read / write / subscribe to the same project tree. |
| `acowork-doc`  | Online document library with real-time multi-user editing (Tiptap + Yjs). |
| `acowork-embed` / `acowork-lsp-relay` / `acowork-vault` / `acowork-sqlite` / `acowork-sign` | Embedding model runner, LSP relay, encrypted KV store, storage backend, package signing — supporting tiers. |

### Standalone vs. cluster

- **Standalone** — Gateway auto-spawns one local Node (`acowork-node --mode local`) on the same host. Best for
  personal use, demos, and edge devices. One process tree, one MQTT broker, one HTTP entry.
- **Cluster** — install `acowork-node` on every host you want Runtimes to live on (`GPU box`, `workstation`,
  `cloud VM`), enroll them with the Gateway's MQTT broker, and the same protocol path lights up. Gateway has
  **zero local-vs-remote code branches** — see
  the project's design docs for the rationale and frozen ceilings.

### System Architecture

<p align="center">
  <img src="./assets/architecture.svg" alt="ACowork.AI System Architecture" width="100%" />
</p>

---

## 🚀 Quick Start

Cross-platform build scripts under [`dev/`](./dev/) handle ONNX Runtime discovery, profile switching, and resource
staging — prefer them over calling `cargo` directly.

### Prerequisites

| Tool         | Version     | Notes                                                                                                              |
| ------------ | ----------- | ------------------------------------------------------------------------------------------------------------------ |
| Rust         | **nightly** | `rustup default nightly`                                                                                           |
| Node.js      | >= 18       | Desktop App and Tauri CLI                                                                                          |
| PowerShell   | 7.x         | Required on Windows (`.ps1` scripts); `pwsh` recommended                                                          |
| ONNX Runtime | auto-managed | Installed by `dev/setup_ort.*` into `.ort/onnxruntime-<plat>-<arch>-<ver>/`                                       |

```bash
git clone https://github.com/tranxon/ACowork.git
cd ACowork
```

### Step 1 — Install ONNX Runtime (one-time)

```bash
# Windows PowerShell
.\dev\setup_ort.ps1

# macOS / Linux / WSL / Git Bash
./dev/setup_ort.sh
```

### Step 2 — Build & start the backend (Gateway + Runtime + Node)

```bash
# Windows — release build, then start Gateway
.\dev\build_core.ps1 -Start

# macOS / Linux — release build + start
./dev/build_core.sh

# Debug profile
.\dev\build_core.ps1 -Debug -Start      # Windows
./dev/build_core.sh --debug              # bash
```

macOS users on Apple Silicon can also use `./dev/build_macos.sh` for a one-click build with CoreML enabled.

### Step 3 — Launch the Desktop App

The Desktop App is a Tauri v2 shell — the React/TS frontend talks to the Gateway over HTTP, while the Rust side
handles the system tray and the MQTT client that subscribes to real-time events.

```bash
cd apps/acowork-desktop
npm install

# Browser-only dev server
npm run dev               # → http://localhost:5173

# Or full Tauri desktop window
npm run tauri dev
```

### ✍️ Try it: write a manifest in 30 seconds

```toml
# examples/qa-agent/manifest.toml
[package]
id = "com.example.qa-agent"
name = "Quality Assurance"
display_name = "QA-Tom"
role = "QA"
version = "1.0.0"

[llm]
provider = "deepseek"
model = "deepseek-v4-flash"

[permissions]
tools = ["rag_query", "read_file", "write_file"]
```

```markdown
<!-- prompts/system.md -->
You are a QA Agent, helping users with quality management and code review.
```

Then build & sign:

```bash
./dev/build-agent.sh examples/qa-agent   # produces com.example.qa-agent.agent
```

> **Status**: ACowork is in **alpha**. The Gateway, Runtime, Grafeo memory engine, and Desktop App are under active
> development. See [Roadmap](#-roadmap) for what is shipping today and what is next.

For more — packaging installers, signing, CI, remote-node onboarding — see [`docs/design/zh/`](./docs/design/zh/).

---

## 🧪 Agent Development Workflow

```
① Authoring       manifest.toml + prompts/ + skills/SKILL.md + optional tools/*.wasm
② Signing         acowork-keygen → acowork-sign  (developer key, signed package)
③ Debugging       Desktop App DevMode → conversational debug, SKILL.md hot-reload, breakpoints, recording/replay
④ Publishing      Publishing wizard → remote registry, or share the .agent file directly
```

Developers build agents by **tuning declarative configurations** — system prompts, tool capabilities, memory behavior —
not writing imperative code. The whole pipeline from authoring to publishing is supported by the platform.

---

## 📈 Roadmap

| Phase | Scope                                                                                                                                            | Status         |
| ----- | ------------------------------------------------------------------------------------------------------------------------------------------------ | -------------- |
| 1     | Foundation + LLM interaction (MVP): package parsing, signing, Runtime main loop, Gateway basics                                                  | ✅ Done         |
| 2     | Memory layering + multi-user accounts: Grafeo biomimetic layers, instant extraction, associative diffusion; per-user sessions & audit | ✅ Done         |
| 3     | AI-native PM & Doc: `acowork-pm` and `acowork-doc` standalone services, REST + MCP, multi-user real-time editing (Tiptap + Yjs) | 🚧 In progress |
| 4     | Distributed runtime: Node Agent enrollment, cluster mode, shared protocol path | 🚧 In progress |
| 5     | Permissions & sandbox: filesystem isolation, WASM sandbox (Wasmtime), Approval Gate                                                             | 🚧 Partial     |
| 6     | Desktop App + dev framework: Debug Protocol, Skill hot-reload, recording/replay; MQTT-based IPC                                                | 🚧 In progress |
| 7     | Ecosystem: remote `.agent` registry, Agent store, Memory Sync across hosts                                                                     | 🔮 Planning    |

---

## 📚 Documentation

- Architecture design: [`docs/design/zh/`](./docs/design/zh/)
- Module-level design: [`docs/module-design/zh/`](./docs/module-design/zh/)
- Design notes & decision history: [`docs/adr/zh/`](./docs/adr/zh/)
- Developer conventions: [`AGENTS.md`](./AGENTS.md)

---

## 🧪 References & Acknowledgments

ACowork.AI's design draws inspiration from:

- [ZeroClaw 🦀](https://github.com/zeroclaw-labs/zeroclaw) — trait-driven runtime, security decorators, streaming parser
- [Grafeo](https://github.com/GrafeoDB/grafeo) — HNSW vector index, BM25 full-text search, hybrid search
- [Mem0](https://github.com/mem0ai/mem0) — multi-level memory, user/session/Agent state
- [HippoRAG](https://github.com/OSU-NLP-Group/HippoRAG) — neurobiology-inspired long-term memory, associative diffusion
- [LightMem](https://github.com/zjunlp/LightMem) — lightweight memory compression
- [OpenCode](https://github.com/anomalyco/opencode) — multi-agent collaboration, provider-agnostic design

---

## 🤝 Contributing

The project is in **active implementation (Alpha)**. Code, design feedback, and reviews are all welcome:

- Open issues for bug reports, proposals, or design feedback
- Read [`AGENTS.md`](./AGENTS.md) for project conventions before opening a PR

---

## 📄 License

Apache-2.0 — see [`LICENSE`](./LICENSE) for details.

---

<p align="center">
  <b>ACowork.AI — Collaborate with your AI Colleagues</b><br>
  <i>Build and collaborate with AI agents like team members.</i>
</p>
