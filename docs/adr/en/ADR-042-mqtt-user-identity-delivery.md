# ADR-042: User Identity Delivered over an MQTT Global Resource Topic

> **Chinese source of truth**: [ADR-042](../zh/ADR-042-mqtt-user-identity-delivery.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Draft

## Date

2026-07-21

## Decision Makers

大鱼 (Dayu)

## Related

- [ADR-016](./ADR-016-centralized-exception-handling.md) — IPC gRPC migration
- [ADR-033](./ADR-033-mqtt-replace-grpc-websocket.md) — MQTT replaces gRPC and WebSocket
- [ADR-040](./ADR-040-runtime-adapter-use-case-layer.md) — removed the gRPC hello_config path
- [ADR-011](./ADR-011-compaction-as-distillation.md) — introduced the `identity_context` injection mechanism

---

## 1. Decision summary

ADR-040 removed the gRPC `hello_config` path, so the Runtime no longer pulls the
UserProfile from the Gateway. The MQTT rework in ADR-033 **never added a replacement
delivery channel**, leaving `identity_context = None` at Runtime startup:

```rust
// core/acowork-runtime/src/startup/agent_init.rs:568-571
// ADR-040: gRPC hello_config path removed. User identity is not yet
// available via MQTT; context builder is created without identity.
let identity_context: Option<String> = None;
```

An empty `identity_context` means the compaction system prompt never sees the user
language preference (`Language: zh-CN`), so the compact model defaults to English output —
observed directly: a conversation such as "Hi, test whether the user message lands after the
context" was compacted into an English summary.

This ADR adds a new topic **`acowork/global/user_profile`** (retained, QoS 1) carrying the
active user profile as a retained snapshot. Runtimes subscribe, write it into
`identity_context`, and additionally receive a hot push at runtime so every Runtime sees a
profile switch immediately.

**Three core decisions**

1. **Single owner** — the Gateway is the sole authority for UserProfile (it holds the
   Vault, the persisted file, and the active user). The Runtime never owns a profile and
   may only subscribe.
2. **`AvailableUsers` wrapper for the payload** — matching the naming of the sibling
   messages (`AvailableProviders`, `AvailableMcps`); a `version` field guards against
   out-of-order delivery.
3. **Only the active user is published, not the whole table** — sufficient for the
   single-user phase. The multi-user phase is handled by the reserved
   `acowork/users/{user_id}/...` namespace.

## 2. Root cause

The gRPC era worked end to end:

```
Desktop
  │ POST /api/users (CRUD)
  ▼
Gateway (UserProfile in resource_cache.user_profile_list)
  │ gRPC hello_config (PushUserProfile)  ← the old path
  ▼
Runtime (IdentityContext = format_user_profile_context(profile))
  ▼
ContextBuilder → SessionState.identity_context → compaction system prompt
```

Then three changes each removed a link without restoring it. ADR-033 replaced gRPC and
WebSocket with MQTT but only covered two data classes — event streams and live status
sync; a startup-time one-shot config like UserProfile was never mapped. ADR-040
deleted the gRPC server and `connect_gateway_client` during dead-code cleanup **without
realizing UserProfile went with it**: `resource_pusher.rs::push_user_profile` degraded into a
no-op stub and all four call sites in `users_api.rs` became no-ops.

| Link | gRPC era | After ADR-040 |
|------|----------|---------------|
| Gateway persists UserProfile | yes, `user_profiles.json` | still written |
| Gateway HTTP CRUD | `/api/users/*` | still works |
| Gateway → Runtime delivery | `hello_config` | **broken** |
| Runtime `identity_context` | filled at startup | **always None** |
| Compaction language hint | follows the profile language | always English |

The same gap is visible across the whole stack: `mqtt.md` §3.1.1 lists five resource
types and not user_profile; the §7.4 matrix has no such row; `mqtt_payload.proto` defines
five available-resource messages and no user one; `topics::USER_PROFILE` does not exist;
`AvailableResourceCache` has five fields; and `agent_init.rs` hardcodes `None`.

## 3. Decision

**3.1 New topic**

```
acowork/global/
├── ...the 5 existing resources...
└── user_profile            # [Retained, QoS 1] snapshot of the active user profile
                            # payload = AvailableUsers {
                            #   version: u64,        // mirrors user_profile_list.version
                            #   active_user: UserProfileRef {  // empty = no active user
                            #     user_id, display_name, language, timezone,
                            #     city?, country?, occupation?, communication_style?,
                            #     custom_json,
                            #   },
                            # }
```

**Owner**: the Gateway. Its background publisher loop recomputes the payload and
PUBLISHes with `retain=true` whenever `user_profile_list.version` changes or the active
user switches. **Subscribers**: every Runtime — `SUB acowork/global/#` already exists at
`client.rs:455`, so no new subscription is needed. **QoS 1**, consistent with the other
§3.1.1 topics, because a state change must not be lost.

**3.2 Payload**

```protobuf
message AvailableUsers {
  uint64 version = 1;                // mirrors user_profile_list.version
  UserProfileRef active_user = 2;    // empty = no active user
}

message UserProfileRef {
  string user_id = 1;
  string display_name = 2;
  string language = 3;               // BCP 47 (e.g. "zh-CN", "en-US")
  string timezone = 4;               // IANA (e.g. "Asia/Shanghai", "UTC")
  optional string city = 5;
  optional string country = 6;
  optional string occupation = 7;
  optional string communication_style = 8;
  string custom_json = 9;            // HashMap<String, String> serialized as JSON
}
```

Field pruning: `avatar` / `builtin_avatar` are pure UI concerns; `created_at` /
`updated_at` serve the admin UI; `is_active` is always true on this topic since only the
active user is published, so it is dropped; and `custom` is a JSON string to avoid
embedding a `map<string,string>` in protobuf.

**3.3 Runtime startup wait**

```
1. SUBSCRIBE acowork/global/#        (already in the ADR-039 bootstrap)
2. wait for the retained acowork/global/user_profile (timeout 5s)
3. format_user_profile_context() → identity_context
```

**On timeout** — if nothing arrives within 5s (the Gateway is not up yet, or no
profile is installed), fall back to `identity_context = None`, matching current
behaviour. A late-arriving retained message then reaches every active session through
`SessionMessage::UpdateIdentityContext` (see §3.4).

**3.4 Runtime hot-push routing**

```
Gateway PUBLISH acowork/global/user_profile (retain=true)
  ▼
Runtime MQTT event loop
  │ decode AvailableUsers → take active_user
  │ format_user_profile_context() → identity_context: Option<String>
  ▼
SessionManager.update_user_identity(profile)
  │ broadcast to every session ContextBuilder
  ▼
SessionMessage::UpdateIdentityContext { identity_context }
  ▼
session_task.rs:1368 — syncs context_builder + session.identity_context
```

This reuses the existing `UpdateIdentityContext` route (`session_task.rs:108-109` plus
`session_manager.rs:1740-1746`) with zero intrusion.

**3.5 Change surface**

| File | Change |
|------|--------|
| `core/acowork-core/proto/mqtt_payload.proto` | add `AvailableUsers` / `UserProfileRef`; add `available_users` to `DataEnvelope.payload` as field 15 (10-14 stay with the 5 existing types) |
| `docs/protocols/en/mqtt.md` §3.1.1, §7.4 | add the `user_profile` tree row and matrix row |
| `acowork-gateway/src/mqtt/global_resources_publisher.rs` | `topics::USER_PROFILE`, `publish_user_profiles()`, `build_available_users()`, and a call from `publish_all()` |
| `acowork-gateway/src/http/users_api.rs` | the 4 `state.pusher.push_user_profile()` calls become `state.mqtt_publisher_trigger.trigger()` |
| `acowork-runtime/src/mqtt/available_cache.rs` | add `user_profile: Option<AvailableUsers>` and an `update_from_mqtt` branch |
| `acowork-runtime/src/startup/agent_init.rs` | drop the hardcoded `None`; wait up to 5s on the cache, take the active user, build `identity_context` |
| `acowork-runtime/src/mqtt/client.rs` | call `session_manager.update_user_identity()` when the topic arrives |
| `agent/session/session_manager.rs` | `update_user_identity` already exists and already accepts `Option<UserProfile>` — no change |

The `UpdateIdentityContext` handling at `session_task.rs:1368` needs no change; it already
broadcasts across all sessions (the loop at `session_manager.rs:1740-1746`).

## 4. Rejected alternatives

**B — HTTP late-bind through a shared cache:** the Runtime would `GET /api/users/active`
once at startup. Rejected because it breaks the "pub/sub per data source" principle (a
UserProfile is a low-churn authoritative snapshot suited to retained, not a full list),
adds a startup failure mode, and still needs a different path for the later hot push at
`users_api.rs:267`, splitting the mechanism.

**C — put UserProfile into `agents/{id}/config`:** rejected on data ownership — UserProfile
is user-level data and must not be scattered across per-agent config; it breaks entirely
under user switching and introduces cross-agent synchronization problems.

## 5. Verification

**Unit**

- `MqttGlobalResourcesPublisher::test_publisher_publishes_retained_snapshot` already covers
  the `acowork/global/#` wildcard subscription; add an assertion that the published
  topics contain `acowork/global/user_profile`.
- Extend the `AvailableResourceCache::test_update_from_mqtt_providers` pattern with
  `test_update_from_mqtt_user_profile`.

**Integration**

- Manually compact a CJK conversation and expect a **Chinese** summary, not English.
- Switch the active profile in Desktop Settings and expect every Runtime to receive the
  new profile without an agent restart.

**Regression**

- Single user with no profile: after the 5s timeout `identity_context` is None and
  compaction falls back to conversation-language detection (the v6 prompt); behaviour is
  unchanged.
- Multiple Runtimes: each receives the retained message and caches it independently.
- Reconnect: `run_bootstrap()` redoes the §3.1.1 subscriptions and retained messages are
  redelivered automatically.

## 6. Out of scope

- **Multi-user ACL isolation** — the §10 `acowork/users/{user_id}/` namespace is reserved but
  not handled here.
- **Runtime writing UserProfile back to the Gateway** — a Runtime only ever reads it; CRUD
  stays on the Desktop HTTP path.
- **Publishing avatar / builtin_avatar** — UI-only, not worth protocol surface.
- **Compatibility with the old gRPC `PushUserProfile` payload** — ADR-040 already deleted it,
  and it is not retained.

## 7. Follow-ups

- [ ] Test compaction fallback when the active profile is None (the v6 prompt
  "fall back to identity when the conversation is ambiguous").
- [ ] When the ADR-033 §3.4 multi-user topics activate, decide whether `user_profile` becomes
  `acowork/users/{uid}/profile` or stays global — this depends on which user view each
  Runtime has in the multi-user phase.
