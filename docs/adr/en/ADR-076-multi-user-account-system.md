# ADR-076 — Multi-User Account System

> **Translation status**: English translation is incomplete. The Chinese
> version (`docs/adr/zh/ADR-076-multi-user-account-system.md`) is the
> authoritative source. This English file currently covers only §决策 12
> v2 and §决策 12 v3 (the first-boot setup amendments).
>
> For sections not yet translated, please read the Chinese source. The
> section headings and decision numbers are identical.

## § Decision 12 v2: Empty account store no longer refuses to boot — seed a passwordless admin + restricted mode

### Revision motivation (implementation-period note)

The original § Decision 12 rule
("multi_user + empty account store → fail-fast") placed the burden of
"create the first admin" on the operator via the
`[multi_user].bootstrap_admin` TOML section. The flow in practice:

1. New operator runs `build_macos.sh --start --remote`.
2. `--remote` binds `0.0.0.0` → the Gateway infers `AUTH_MODE=multi_user`.
3. No TOML has `bootstrap_admin` (it's the install default) → Gateway
   refuses to start.
4. The wrapper script redirects stderr to `/dev/null` (so the failure
   message is invisible).
5. The Desktop cannot connect. The operator sees a black hole.

This is a bad first-use path — equivalent to "the product cannot be
used". The fix in v2 keeps the security invariant ("operator must set
a password before serving") but moves the *mechanism* from "must edit
TOML" to "interactive TTY prompt at first startup" or "a single CLI
subcommand".

### New contract (v2, implemented; see §6.4 in the zh version for source-file references)

1. **Empty account store + no TOML `bootstrap_admin`** → seed a
   passwordless account with `role=Admin` and
   `password_hash=DISABLED_PASSWORD_HASH`. Enter **restricted mode**
   (`is_restricted()` returns `true`).
2. **Restricted-mode HTTP behaviour**:
   - `GET /health` → 200.
   - `GET /api/status` → 200 with a new field `requires_setup: true`.
   - Every other `/api/*` → **403 `{error: "setup_required"}`** (not
     401, by design).
3. **Restricted-mode release**: the operator completes first-boot
   setup on the Gateway host. Restricted mode ends; normal service
   resumes. **No HTTP endpoint accepts the first password.** Passwords
   travel only via stdin / file / TOML — they **never cross the
   network**.
4. **Empty account store + TOML `bootstrap_admin` configured** →
   behaviour unchanged (use the TOML password directly, skip
   restricted mode). Kept as the "zero-interactive boot" path for
   non-interactive / container deployments.
5. **Non-empty account store** → behaviour unchanged (TOML
   `bootstrap_admin` is ignored with a `warn` log).

### Three first-boot setup paths (ordered by usage frequency)

| Path | Use case | Implementation |
|---|---|---|
| **TTY prompt** (auto, on any subcommand-less invocation) | Local developers, ssh remote | `cli.rs`: after `Gateway::new`, if `restricted` **and both `stdin` and `stdout` are TTYs** → `rpassword::prompt_password` twice (confirm) → write accounts.json. A non-TTY start is **not** aborted (v3, see § Decision 12 v3). |
| **`admin-setup` CLI subcommand** | systemd / Docker / non-TTY | `acowork-gateway admin-setup [--password-file PATH] [--password-stdin]` → reads password from file / stdin / `rpassword` prompt → `AuthService::set_admin_password` → **does not start the Gateway**, exits 0 |
| **`[multi_user].bootstrap_admin` TOML section** | Pure config-driven (k8s ConfigMap / image bake) | Restart the daemon → `ensure_bootstrap_admin` walks path A → restricted mode is skipped from boot |

### Security invariants (v2 breaks none)

- **The first password never crosses the network.** No HTTP endpoint
  accepts it. A LAN attacker racing to connect only reaches
  `/health` and `/api/status`, which do not accept passwords. SSH /
  physical access = already admin.
- **Restricted mode + LAN attacker + remote Desktop**: the Desktop
  reads `requires_setup=true` → renders a "go set the password on the
  Gateway host" gate, **does not** attempt to log in. The attacker
  cannot set the password on the operator's behalf.
- **Race window**: from "`set_admin_password` atomic write of `accounts.json`" to "next middleware read of `is_restricted()` returns `false`" is microseconds (same-process file I/O + middleware calls `is_restricted()` directly).
- **Policy parity**: `set_admin_password` runs `PasswordPolicy::validate`, sharing the policy path with normal `change-password`.

### Relationship to original § Decision 12

- Original "fail-fast" was v1's "safety first" extreme — assuming
  operators will read the TOML. **Empirically false**: `build_macos.sh`
  swallows stderr. v2 keeps the invariant ("operator must set a
  password") while dropping the *mechanism* from "must read docs and
  edit TOML" to "one TTY prompt on gateway startup" or "one CLI
  command".
- The `[multi_user].bootstrap_admin` TOML section is **kept** as the
  compliant escape hatch for "pure config-driven" deployments (CI / k8s
  / image bake).
- v2 is a **downgrade** of v1, not a replacement: the bind-inference
  rule, all `local`-mode no-ops, the UserAccount-in-local-mode
  semantics, the rollback / downgrade path, the mainstream references,
  and the `ponytail` ceiling all carry over.

### Implementation mapping

| File | Change |
|---|---|
| `auth/service.rs::ensure_bootstrap_admin` | Split into Path A (TOML bootstrap_admin → real-hash admin) and Path B (empty store → passwordless admin); keep the "non-empty store → bootstrap_admin ignored" branch |
| `auth/service.rs::set_admin_password` + `is_restricted` | New methods; `set` validates policy, refuses to overwrite; `is_restricted` is the single source for middleware and `requires_setup` |
| `gateway/mod.rs::is_first_boot_restricted` + `set_admin_password` | Public façades used by the daemon arm |
| `cli.rs::Commands::AdminSetup` | New subcommand, three password sources, **does not start the Gateway** |
| `cli.rs` daemon arm | `is_first_boot_restricted()` → prompt only when `stdin` **and** `stdout` are TTYs; **a non-TTY start or a failed prompt only warns (stderr + log file) and does not abort the boot** — the daemon starts HTTP in restricted mode. See § Decision 12 v3 |
| `http/restricted_mode.rs` | New middleware, mounted before `auth_middleware`, returns 403 `setup_required` |
| `http/routes.rs::SystemStatusResponse` | New `requires_setup: bool` field; serialised automatically by `/api/status` |
| Desktop `authStore` + `SetupRequiredView` | Detect `requires_setup=true` → switch to `setup_required` state + 5 s poll of `/api/status` → on flip, auto-`init()` to resume the normal flow |

### Ceilings / explicitly not done

- **Remote Desktop cannot set the first password itself** — by SSH /
  `admin-setup` / TOML only. This is design, not a bug (security
  invariant 1).
- **Poll interval is hard-coded to 5 s.** End-to-end UX lag after
  setup completes ≤ 5 s. Sub-second requires WebSocket / SSE —
  deliberately not done.
- **Restricted mode does not restrict MQTT.** The current
  `mqtt.auth_enabled` CONNECT check is sufficient — a passwordless
  admin cannot mint a token and so cannot connect to the broker. Adding
  a broker-side ACL is the natural upgrade path; not done now (the
  current credential reuse makes it redundant).

## § Decision 12 v3: first-boot setup is decoupled from daemon start — the daemon always starts HTTP first

### Revision motivation (implementation-period note; `cli.rs` refers to this as "v3")

v2 wrote the first-boot entry point as "`cli.rs` **daemon arm**: TTY detection
+ prompt + write; non-TTY **exits 1**". In practice that implementation never
fixed its own motivating scenario — restricted mode was unreachable on the
first-boot path at all:

1. `build_macos.sh --start` launches with `"$GATEWAY_EXE" ... &`. A background
   job in a non-interactive shell gets stdin assigned to `/dev/null` (POSIX;
   verified under a pty), so `stdin.is_terminal() == false` → the "non-TTY"
   branch → **`return Err` → exit 1, HTTP never listens**. The Desktop still
   sees "connection refused" — the same black hole as v1, just with an
   invisible seed in accounts.json.
2. Even with a TTY stdin, the prompt runs *before* `if self.daemon {
   async_main(...) }` and blocks: by the time HTTP starts, the password has
   been written and `is_restricted()` is already `false`. In other words
   **restricted mode never actually served a request** — the
   `requires_setup` / `SetupRequiredView` / 5 s-poll machinery could only
   appear at runtime if a `reset_password` hit (a different failure entirely,
   see the `is_restricted()` definition in § Decision 12 v2).

### v3 contract: first-boot setup is **best effort** and never aborts the boot

1. When `is_first_boot_restricted()` is true, the prompt is shown only if
   **both** `stdin` and `stdout` are TTYs. `rpassword` reads *and* writes
   `/dev/tty` (not fd 0/1), so "both stdio ends are terminals" is the
   conservative proxy for "a human is attached"; a redirected start (build
   script / systemd / Tauri child) never prompts.
2. A successful prompt writes the password; without `--daemon` the process
   still exits so the operator chooses when to serve (v2 semantics kept).
3. **Prompt unavailable (non-TTY) or failed (mismatch / policy) only warns**
   — `eprintln!` for a human plus `tracing::warn!` for the log file, naming
   the three setup paths — and then boots anyway. With `--daemon`, HTTP starts
   and restricted mode **serves**.
4. Restricted mode is therefore genuinely reachable: `/health` and
   `/api/status` (with `requires_setup: true`) answer 200, every other
   `/api/*` answers 403 `setup_required`; the Desktop renders the gate and
   polls every 5 s, and once the operator runs `admin-setup` on the host the
   next middleware read of the store clears it — **no restart**.

Measured (`--home <tmp> --auth-mode multi_user --daemon --addr
127.0.0.1:21999`, all stdio redirected):

| Request | Result |
|---|---|
| process | stays alive (v2: exit 1) |
| `GET /health` | 200 |
| `GET /api/status` | 200 + `requires_setup: true` |
| `GET /api/users` (no token) | **403 `setup_required`** (not 401 — see the fixed bug below) |
| `POST /api/auth/login` | 403 `setup_required` |
| after `admin-setup --password-stdin` | `requires_setup: false`, login returns a token pair, `/api/users` is back to 401 |

> **Bug fixed in this round**: the `restricted_mode` middleware was
> `.layer()`-ed *before* `auth_middleware`, and axum runs layers
> bottom-to-top (last added = outermost) — so it actually sat *inside* the
> auth gate, and an unauthenticated restricted-mode request was answered 401
> by the auth gate, contradicting the v2 contract ("403, not 401"). It is now
> mounted outside the auth gate, with a regression test that goes through the
> real `build_router` (`real_router_answers_403_not_401_without_a_token`).

## Translation completeness

| Section | Status |
|---|---|
| § Decision 12 v2 (this file) | ✅ translated |
| § Decision 12 v3 (this file) | ✅ translated |
| All other sections | ⏳ Chinese-only, please read `docs/adr/zh/ADR-076-multi-user-account-system.md` |