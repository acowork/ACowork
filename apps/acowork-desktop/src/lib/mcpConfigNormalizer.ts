//! MCP config input normalizer.
//!
//! Converts the formats an MCP vendor documents (a vendor `mcpServers` JSON
//! block, a single server JSON object, or a CLI `mcp add` command line) into
//! the one internal shape the Gateway catalog accepts —
//! `McpServerConfigDef`. The UI never consumes this output directly as a
//! submission: the dialog autofills its form fields from it so the user
//! always reviews before writing (per the "fail loud, never guess" rule —
//! new vendor formats extend the alias table below, never the form).
//!
//! Three layers, all deterministic (no regex heuristics, no inference):
//!  1. Format sniffing  — JSON vs CLI, decided by syntax class alone.
//!  2. Structural match — key-presence classification + an alias table
//!     (data, not code) mapping vendor field names onto the closed
//!     `McpServerConfigDef` field set.
//!  3. Exit validation  — the result MUST satisfy the transport's
//!     invariants before it is surfaced; otherwise a structured error is
//!     returned so the dialog can show what failed.

import type { McpServerConfigDef, McpTransportDef } from "./types";

// ── Public types ───────────────────────────────────────────────────────

/** Normalized output plus the diagnostics the dialog surfaces. */
export interface NormalizeResult {
  /** Parsed servers (empty when `errors` is non-empty). A multi-server
   *  `mcpServers` block yields one entry per server. */
  servers: McpServerConfigDef[];
  /** Structured errors — blocks apply. */
  errors: NormalizeError[];
  /** Non-blocking warnings (e.g. an unexpanded `${VAR}` placeholder). */
  warnings: string[];
}

export interface NormalizeError {
  code:
    | "invalid_json"
    | "unrecognized_format"
    | "unsupported_transport"
    | "invalid_combination"
    | "cli_parse";
  message: string;
}

/** Internal structured-error channel (thrown inside the mapping helpers,
 *  caught once at the entry point) so error propagation never interleaves
 *  with the happy-path return plumbing. */
class NormalizeException extends Error {
  constructor(public code: NormalizeError["code"], message: string) {
    super(message);
  }
}

// ── Entry point ────────────────────────────────────────────────────────

/** Normalize any supported vendor input. `fallbackName` is used when the
 *  input carries no name (bare CLI commands); the dialog passes its Name
 *  field, and an empty `fallbackName` becomes an explicit error. */
export function normalizeMcpConfigInput(raw: string, fallbackName: string): NormalizeResult {
  try {
    return normalizeInner(raw, fallbackName);
  } catch (e) {
    if (e instanceof NormalizeException) {
      return { servers: [], errors: [{ code: e.code, message: e.message }], warnings: [] };
    }
    throw e;
  }
}

function normalizeInner(raw: string, fallbackName: string): NormalizeResult {
  const text = raw.trim();
  if (!text) return err("unrecognized_format", "Input is empty.");

  if (text.startsWith("{") || text.startsWith("[")) {
    let value: unknown;
    try {
      value = JSON.parse(text);
    } catch (e) {
      return err("invalid_json", `JSON parse failed: ${e instanceof Error ? e.message : String(e)}`);
    }
    return normalizeJsonValue(value, fallbackName);
  }
  return normalizeCli(text, fallbackName);
}

function err(code: NormalizeError["code"], message: string): NormalizeResult {
  return { servers: [], errors: [{ code, message }], warnings: [] };
}

// ── Layer 2 (JSON family): structural match + alias table ─────────────

/** Container keys that wrap a map of `{ name: entry }` server objects. */
const WRAPPER_KEYS = ["mcpServers", "mcp_servers", "servers"];

/** Per-entry type values → McpTransportDef (data, not code). Keys are the
 *  lowercased, punctuation-stripped spellings vendors actually use. */
const TYPE_ALIASES: Record<string, McpTransportDef> = {
  stdio: "stdio",
  npx: "stdio",
  command: "stdio",
  http: "http",
  streamablehttp: "http",
  streamable: "http",
  sse: "sse",
  httpsse: "sse",
};

function mapTypeAlias(rawType: string): McpTransportDef | undefined {
  const key = rawType.toLowerCase().replace(/[-._\s]/g, "");
  return TYPE_ALIASES[key];
}

function normalizeJsonValue(value: unknown, fallbackName: string): NormalizeResult {
  const warnings: string[] = [];

  if (!isRecord(value)) {
    return err("unrecognized_format", "Top-level JSON must be an object (mcpServers wrapper, { name: entry } map, or a single server object).");
  }

  // Shape 1: vendor config wrapped in `mcpServers` / `mcp_servers` / `servers`.
  for (const wk of WRAPPER_KEYS) {
    if (isRecord(value[wk])) {
      return mapEntries(objectOfEntries(value[wk] as Record<string, unknown>), fallbackName, warnings);
    }
  }
  const values = Object.values(value);
  // Shape 2: a bare map of `{ name: entry }` (all values are objects, and it
  // is not itself a single entry — single entries carry command/url/type/args).
  if (
    values.length > 0 &&
    values.every(isRecord) &&
    !("command" in value) && !("url" in value) && !("type" in value) && !("args" in value)
  ) {
    return mapEntries(objectOfEntries(value), fallbackName, warnings);
  }
  // Shape 3: a single server entry object.
  if ("command" in value || "url" in value || "type" in value || "args" in value) {
    const one = entryToConfig(str(value["name"]) || fallbackName, value);
    warnings.push(...collectPlaceholderWarnings({ ...one.env, ...one.headers }));
    return finish([one], warnings);
  }
  // Shape 4: single entry nested one level under an anonymous key.
  if (values.length === 1 && isRecord(values[0])) {
    const nested = values[0] as Record<string, unknown>;
    if ("command" in nested || "url" in nested) {
      const one = entryToConfig(fallbackName, nested);
      warnings.push(...collectPlaceholderWarnings({ ...one.env, ...one.headers }));
      return finish([one], warnings);
    }
  }
  return err("unrecognized_format", "JSON is not a recognized MCP config shape (expected mcpServers wrapper, { name: entry } map, or a single server object).");
}

function objectOfEntries(obj: Record<string, unknown>): [string, Record<string, unknown>][] {
  const out: [string, Record<string, unknown>][] = [];
  for (const [k, v] of Object.entries(obj)) {
    if (isRecord(v)) out.push([k, v]);
  }
  return out;
}

function mapEntries(
  entries: [string, Record<string, unknown>][],
  fallbackName: string,
  warnings: string[],
): NormalizeResult {
  if (entries.length === 0) return err("unrecognized_format", "No server entries found in the pasted config.");
  const servers = entries.map(([name, entry]) => entryToConfig(name || fallbackName, entry));
  for (const s of servers) warnings.push(...collectPlaceholderWarnings({ ...s.env, ...s.headers }));
  return finish(servers, warnings);
}

/** Map one vendor entry onto McpServerConfigDef. Unimplementable features
 *  (OAuth) fail loud instead of silently dropping credentials; unknown
 *  noise keys are dropped. */
function entryToConfig(name: string, entry: Record<string, unknown>): McpServerConfigDef {
  const cfg: McpServerConfigDef = {
    name,
    transport: "stdio",
    command: "",
    args: [],
    env: {},
    headers: {},
  };

  const rawType = str(entry["type"]) ?? str(entry["transport"]);
  if (rawType) {
    const mapped = mapTypeAlias(rawType);
    if (!mapped) {
      throw new NormalizeException("unsupported_transport", `Transport type "${rawType}" is not supported (stdio / http / sse only).`);
    }
    cfg.transport = mapped;
  } else {
    cfg.transport = entry["url"] ? "http" : "stdio";
  }

  // Explicit auth config → fail loud (OAuth is a Runtime protocol feature,
  // not a config-normalization one; see the static-header downgrade advice).
  const auth = entry["auth"] ?? entry["authType"] ?? entry["auth_type"];
  if (typeof auth === "string" && auth !== "none" && auth !== "") {
    throw new NormalizeException(
      "unsupported_transport",
      `Auth mode "${auth}" is not supported yet. If the vendor offers a static API key, pass it via headers ({"Authorization": "Bearer <key>"}) or env instead.`,
    );
  }

  const command = str(entry["command"]);
  if (command) cfg.command = command;

  const args = arr(entry["args"]);
  if (args) cfg.args = args.map(String);

  cfg.env = stringMap(entry["env"]);
  cfg.headers = stringMap(entry["headers"]);

  const url = str(entry["url"]) ?? str(entry["uri"]);
  if (url) cfg.url = url;

  const timeout = entry["timeout"] ?? entry["tool_timeout_secs"];
  if (typeof timeout === "number" && timeout > 0) cfg.tool_timeout_secs = timeout;

  if (!cfg.name) {
    throw new NormalizeException("invalid_combination", "No server name found — fill the Name field and parse again.");
  }
  return cfg;
}

// ── Layer 2 (CLI family): shell tokenizer + flag consumer ─────────────

/** Shell-like tokenizer: honors single/double quotes and backslash escapes,
 *  so paths with spaces and `--header "K: V"` survive intact. */
export function shellTokenize(input: string): string[] {
  const tokens: string[] = [];
  let cur = "";
  let has = false; // a token started (distinguishes "" from no token)
  let quote: "'" | '"' | null = null;
  for (let i = 0; i < input.length; i++) {
    const ch = input[i];
    if (quote === "'") {
      if (ch === "'") quote = null;
      else cur += ch;
      continue;
    }
    if (quote === '"') {
      // Inside double quotes a backslash only escapes `"` and `\` — Windows
      // paths ("C:\\Program Files\\x") otherwise lose their separators.
      if (ch === "\\" && (input[i + 1] === '"' || input[i + 1] === "\\")) { cur += input[++i]; continue; }
      if (ch === '"') quote = null;
      else cur += ch;
      continue;
    }
    if (ch === "'" || ch === '"') { quote = ch; has = true; continue; }
    if (ch === "\\" && i + 1 < input.length) { cur += input[++i]; has = true; continue; }
    if (/\s/.test(ch)) {
      if (has) { tokens.push(cur); cur = ""; has = false; }
      continue;
    }
    cur += ch;
    has = true;
  }
  if (has) tokens.push(cur);
  return tokens;
}

interface CliFlags {
  transport?: string;
  urlFlag?: string;
  name?: string;
  env: string[];
  header: string[];
}

/** Value-taking flags whose meaning is client-local (install scope, header
 *  of a managed auth flow…) — consume their value and drop it. */
const CLI_SKIP_VALUE_FLAGS = new Set(["-s", "--scope", "--description"]);

/** Consume known `mcp add` flags anywhere in the token stream; every other
 *  token (including a stdio command's own flags such as `npx -y`) is kept
 *  verbatim in `positional` so the launch command survives intact. `hadAdd`
 *  records whether an `add` subcommand verb was present — that marks the
 *  first positional as the server NAME (installer convention) rather than
 *  the command. */
function consumeFlags(
  tokens: string[],
  start: number,
): { flags: CliFlags; positional: string[]; trailing: string[]; hadAdd: boolean; error?: NormalizeResult } {
  const flags: CliFlags = { env: [], header: [] };
  const positional: string[] = [];
  const trailing: string[] = [];
  const hadAdd = start > 0;
  let dashdash = false;
  let error: NormalizeResult | undefined;

  const setValue = (slot: "transport" | "urlFlag" | "name", v: string) => {
    flags[slot] = v;
  };
  const missing = (flagName: string): NormalizeResult =>
    err("cli_parse", `Missing value for ${flagName}.`);

  for (let i = start; i < tokens.length; i++) {
    const tk = tokens[i];
    if (dashdash) { trailing.push(tk); continue; }
    if (tk === "--") { dashdash = true; continue; }

    // Bare `KEY=value` token → env assignment.
    if (!tk.startsWith("-") && /^[A-Za-z_][A-Za-z0-9_]*=/.test(tk)) { flags.env.push(tk); continue; }

    const isLong = tk.startsWith("--");
    const eq = isLong ? tk.indexOf("=") : -1;
    const flagName = eq > 0 ? tk.slice(0, eq) : tk;
    const inlineValue = eq > 0 ? tk.slice(eq + 1) : null;
    const nextValue = (): string | null => {
      if (inlineValue !== null) return inlineValue;
      if (i + 1 < tokens.length) { return tokens[++i]; }
      return null;
    };

    if (CLI_SKIP_VALUE_FLAGS.has(flagName)) { nextValue(); continue; }

    switch (flagName) {
      case "--transport": case "-t": case "--type": {
        const v = nextValue(); if (v === null) { error = missing(flagName); return { flags, positional, trailing, hadAdd, error }; }
        setValue("transport", v); break;
      }
      case "--url": {
        const v = nextValue(); if (v === null) { error = missing(flagName); return { flags, positional, trailing, hadAdd, error }; }
        setValue("urlFlag", v); break;
      }
      case "--name": {
        const v = nextValue(); if (v === null) { error = missing(flagName); return { flags, positional, trailing, hadAdd, error }; }
        setValue("name", v); break;
      }
      case "--env": case "-e": case "--env-var": {
        const v = nextValue(); if (v === null) { error = missing(flagName); return { flags, positional, trailing, hadAdd, error }; }
        flags.env.push(v); break;
      }
      case "--header": case "-H": {
        const v = nextValue(); if (v === null) { error = missing(flagName); return { flags, positional, trailing, hadAdd, error }; }
        flags.header.push(v); break;
      }
      default:
        // Unknown token — a command, an arg, or the command's own flag.
        positional.push(tk);
    }
  }
  return { flags, positional, trailing, hadAdd };
}

function normalizeCli(text: string, fallbackName: string): NormalizeResult {
  const tokens = shellTokenize(text);
  if (tokens.length === 0) return err("cli_parse", "Command is empty.");

  // Skip a leading runner (`qodercli` / `npx ... mcp add`) up to `add`.
  let i = 0;
  const addIdx = tokens.findIndex((tk) => tk === "add");
  if (addIdx >= 0) i = addIdx + 1;

  const { flags, positional, trailing, hadAdd, error } = consumeFlags(tokens, i);
  if (error) return error;

  // Map transport flag (same alias table as the JSON family).
  let transport: McpTransportDef = "stdio";
  let transportExplicit = false;
  if (flags.transport) {
    const mapped = mapTypeAlias(flags.transport);
    if (!mapped) return err("unsupported_transport", `Transport "${flags.transport}" is not supported (stdio / http / sse only).`);
    transport = mapped;
    transportExplicit = true;
  }

  // Positional contract (deterministic, no guessing between an endpoint URL
  // and a command argument that happens to be a URL):
  //  - installer lines (`… add <name> <command|url> …`) put the server name
  //    first; a bare command line has no name slot (falls back to the
  //    dialog's Name field).
  //  - transport is http/sse only when explicitly flagged, given via --url,
  //    or the line is exactly `<name?> <url>` — otherwise stdio.
  let name = flags.name ?? "";
  let core = positional;
  if (hadAdd && core.length > 0) {
    if (core[0].startsWith("-") && !isHttpUrl(core[0])) {
      return err("cli_parse", `Unknown flag "${core[0]}". Paste the command verbatim, or use the form fields instead.`);
    }
    if (!isHttpUrl(core[0])) {
      name = name || core[0];
      core = core.slice(1);
    }
  }

  const urlInCore = core.find(isHttpUrl);
  let url = flags.urlFlag ?? "";
  if (urlInCore) {
    if (url && url !== urlInCore) {
      return err("invalid_combination", "Two different URLs given (a --url flag and a positional).");
    }
    url = urlInCore;
  }

  // Transport inference (see the contract above).
  if (!transportExplicit) {
    if (flags.urlFlag || (urlInCore && core.filter((tk) => !isHttpUrl(tk)).length === 0)) {
      transport = "http";
    }
  }

  let command = "";
  const args: string[] = [...trailing];
  if (transport === "http" || transport === "sse") {
    if (!url) return err("invalid_combination", `Transport "${transport}" requires a URL — include it in the command or fill the URL field.`);
    const extras = core.filter((tk) => tk !== url);
    if (extras.length > 0) {
      return err("invalid_combination", `Transport "${transport}" takes a single URL — unexpected extra arguments: ${extras.join(" ")}`);
    }
  } else {
    if (core.length === 0) return err("invalid_combination", "stdio transport requires a command — check the pasted line or fill the form below.");
    command = core[0];
    args.unshift(...core.slice(1));
  }

  // npx/uvx local-proxy lines (`npx -y mcp-remote <url>`, …) wrap a remote
  // endpoint; a direct http connection is the equivalent native form. Only
  // collapse without an explicit --transport.
  if (
    transport === "stdio" && !transportExplicit &&
    (command === "npx" || command === "uvx") &&
    args.length > 0 && isHttpUrl(args[args.length - 1])
  ) {
    url = args[args.length - 1];
    args.pop();
    command = "";
    transport = "http";
  }

  const env: Record<string, string> = {};
  for (const item of flags.env) {
    const eq = item.indexOf("=");
    if (eq <= 0) return err("cli_parse", `Invalid env "${item}" (expected KEY=VALUE).`);
    env[item.slice(0, eq)] = item.slice(eq + 1);
  }
  const headers: Record<string, string> = {};
  for (const item of flags.header) {
    const colon = item.indexOf(":");
    if (colon <= 0) return err("cli_parse", `Invalid header "${item}" (expected "Name: value").`);
    headers[item.slice(0, colon).trim()] = item.slice(colon + 1).trim();
  }

  const finalName = name || fallbackName;
  const warnings = collectPlaceholderWarnings({ ...env, ...headers });
  const cfg: McpServerConfigDef = {
    name: finalName,
    transport,
    command,
    args,
    env,
    headers,
    url: url || undefined,
  };
  return finish([cfg], warnings);
}

function isHttpUrl(s: string): boolean {
  return /^https?:\/\//i.test(s);
}

// ── Layer 3: exit validation + diagnostics ────────────────────────────

/** `${VAR}` style placeholders survive parsing but cannot be expanded here
 *  (the desktop has no shell environment) — surface them as warnings so the
 *  user replaces them with literal values before saving. */
function collectPlaceholderWarnings(kv: Record<string, string>): string[] {
  const out: string[] = [];
  for (const [k, v] of Object.entries(kv)) {
    if (/\$\{[^}]+\}/.test(v)) out.push(`"${k}" contains an unexpanded placeholder (${v}) — replace it with the actual value.`);
  }
  return out;
}

function finish(servers: McpServerConfigDef[], warnings: string[]): NormalizeResult {
  // Final invariant sweep (mirrors what Runtime create_transport enforces):
  // stdio → command, http/sse → url. A violated combination never reaches
  // the form fields.
  for (const s of servers) {
    if ((s.transport === "http" || s.transport === "sse") && !s.url) {
      return err("invalid_combination", `"${s.name || "server"}": transport ${s.transport} requires a url.`);
    }
    if (s.transport === "stdio" && !s.command) {
      return err("invalid_combination", `"${s.name || "server"}": stdio requires a command.`);
    }
  }
  return { servers, errors: [], warnings };
}

// ── small helpers ──────────────────────────────────────────────────────

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}
function str(v: unknown): string | undefined {
  return typeof v === "string" ? v : undefined;
}
function arr(v: unknown): unknown[] | undefined {
  return Array.isArray(v) ? v : undefined;
}
/** Keys known to be harmless UI-side noise — dropped without warning. */
const NOISE_KEYS = new Set([
  "isenabled", "disabled", "enabled", "alwaysallow", "tools", "autoapprove",
  "startincontext", "description", "icon", "icons",
]);
function stringMap(v: unknown): Record<string, string> {
  const out: Record<string, string> = {};
  if (isRecord(v)) {
    for (const [k, val] of Object.entries(v)) {
      if (typeof val === "string") out[k] = val;
      else if (!NOISE_KEYS.has(k.toLowerCase())) {
        throw new NormalizeException("unrecognized_format", `Field "${k}" must be an object of string values.`);
      }
    }
  }
  return out;
}
