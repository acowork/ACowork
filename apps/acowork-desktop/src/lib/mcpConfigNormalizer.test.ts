//! Fixture-locked tests for the MCP config normalizer.
//!
//! One fixture per vendor input format (the formats vendor docs actually
//! publish). New vendor spellings are added here as a table row — never as
//! a new form field in the dialog.

import { describe, expect, it } from "vitest";
import { normalizeMcpConfigInput, shellTokenize } from "./mcpConfigNormalizer";

describe("normalizeMcpConfigInput — JSON family", () => {
  it("Claude/Cursor-style mcpServers block (stdio)", () => {
    const input = `{
  "mcpServers": {
    "brave-search": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-brave-search"],
      "env": { "BRAVE_API_KEY": "sk-abc" }
    }
  }
}`;
    const r = normalizeMcpConfigInput(input, "");
    expect(r.errors).toEqual([]);
    expect(r.servers).toHaveLength(1);
    expect(r.servers[0]).toMatchObject({
      name: "brave-search",
      transport: "stdio",
      command: "npx",
      args: ["-y", "@modelcontextprotocol/server-brave-search"],
      env: { BRAVE_API_KEY: "sk-abc" },
    });
  });

  it("VS Code streamableHttp entry with headers", () => {
    const input = `{
  "mcpServers": {
    "cmgj": {
      "type": "streamableHttp",
      "url": "https://dashscope.aliyuncs.com/api/v1/mcps/market-cmgjmcp00075722/mcp",
      "headers": { "Authorization": "Bearer sk-real" }
    }
  }
}`;
    const r = normalizeMcpConfigInput(input, "");
    expect(r.errors).toEqual([]);
    expect(r.servers[0]).toMatchObject({
      name: "cmgj",
      transport: "http",
      url: "https://dashscope.aliyuncs.com/api/v1/mcps/market-cmgjmcp00075722/mcp",
      headers: { Authorization: "Bearer sk-real" },
    });
  });

  it("multi-server block yields one entry per server", () => {
    const input = `{
  "mcpServers": {
    "a": { "command": "cmd-a" },
    "b": { "type": "sse", "url": "http://h:1/sse" }
  }
}`;
    const r = normalizeMcpConfigInput(input, "");
    expect(r.servers.map((s) => [s.name, s.transport])).toEqual([["a", "stdio"], ["b", "sse"]]);
  });

  it("single bare entry object (no wrapper)", () => {
    const r = normalizeMcpConfigInput(`{ "command": "uvx", "args": ["docling-mcp-server"], "name": "docling" }`, "fallback");
    expect(r.servers[0]).toMatchObject({ name: "docling", command: "uvx", args: ["docling-mcp-server"] });
  });

  it("bare { name: entry } map without wrapper", () => {
    const r = normalizeMcpConfigInput(`{ "my-srv": { "url": "http://x/y" } }`, "");
    expect(r.servers[0]).toMatchObject({ name: "my-srv", transport: "http", url: "http://x/y" });
  });

  it("noise keys (isEnabled / alwaysApprove) are dropped", () => {
    const r = normalizeMcpConfigInput(
      `{ "mcpServers": { "x": { "command": "c", "isEnabled": true, "alwaysApprove": ["t"] } } }`,
      "",
    );
    expect(r.errors).toEqual([]);
    expect(r.servers[0].command).toBe("c");
  });

  it("unknown type fails loud with unsupported_transport", () => {
    const r = normalizeMcpConfigInput(`{ "command": "c", "type": "websocket" }`, "x");
    expect(r.errors[0]?.code).toBe("unsupported_transport");
    expect(r.servers).toEqual([]);
  });

  it("OAuth-style auth field fails loud with downgrade advice", () => {
    const r = normalizeMcpConfigInput(`{ "url": "https://x/mcp", "auth": "oauth" }`, "x");
    expect(r.errors[0]?.code).toBe("unsupported_transport");
    expect(r.errors[0]?.message).toContain("headers");
  });

  it("http type without url fails with invalid_combination", () => {
    const r = normalizeMcpConfigInput(`{ "type": "http", "command": "" }`, "x");
    expect(r.errors[0]?.code).toBe("invalid_combination");
  });

  it("malformed JSON reports invalid_json", () => {
    const r = normalizeMcpConfigInput(`{ "mcpServers": `, "");
    expect(r.errors[0]?.code).toBe("invalid_json");
  });
});

describe("normalizeMcpConfigInput — CLI family", () => {
  it("qodercli http add with bearer header (the dashscope shape)", () => {
    const input = `qodercli mcp add -s user --transport http market-cmgjmcp00075722 https://dashscope.aliyuncs.com/api/v1/mcps/market-cmgjmcp00075722/mcp --header "Authorization: Bearer sk-literal"`;
    const r = normalizeMcpConfigInput(input, "");
    expect(r.errors).toEqual([]);
    expect(r.servers[0]).toMatchObject({
      name: "market-cmgjmcp00075722",
      transport: "http",
      url: "https://dashscope.aliyuncs.com/api/v1/mcps/market-cmgjmcp00075722/mcp",
      headers: { Authorization: "Bearer sk-literal" },
    });
  });

  it("unexpanded ${VAR} in a header yields a warning, not an error", () => {
    const input = `qodercli mcp add --transport http srv https://x/mcp --header "Authorization: Bearer \${DASHSCOPE_API_KEY}"`;
    const r = normalizeMcpConfigInput(input, "");
    expect(r.errors).toEqual([]);
    expect(r.warnings.join(" ")).toContain("DASHSCOPE_API_KEY");
    expect(r.servers[0]?.headers?.Authorization).toBe("Bearer ${DASHSCOPE_API_KEY}");
  });

  it("stdio CLI: name + command + args + env", () => {
    const r = normalizeMcpConfigInput(`acowork mcp add brave BRAVE_API_KEY=sk-1 npx -y @mcpsrv/brave`, "");
    expect(r.errors).toEqual([]);
    expect(r.servers[0]).toMatchObject({
      name: "brave",
      transport: "stdio",
      env: { BRAVE_API_KEY: "sk-1" },
    });
    // Runner-skipping heuristic: `mcp`/`add` consumed, first positional is
    // the name, the rest is command + args.
    expect(r.servers[0].command).toBe("npx");
    expect(r.servers[0].args).toEqual(["-y", "@mcpsrv/brave"]);
  });

  it("npx mcp-remote <url> collapses to a direct http connection", () => {
    const r = normalizeMcpConfigInput(`npx -y mcp-remote https://api.example.com/mcp`, "remote-srv");
    expect(r.errors).toEqual([]);
    expect(r.servers[0]).toMatchObject({ name: "remote-srv", transport: "http", url: "https://api.example.com/mcp" });
  });

  it("quoted args with spaces survive tokenization", () => {
    const r = normalizeMcpConfigInput(`cmd "C:\\Program Files\\tool srv.exe" --flag`, "n");
    expect(r.servers[0].command).toBe("cmd");
    expect(r.servers[0].args).toEqual(["C:\\Program Files\\tool srv.exe", "--flag"]);
  });

  it("unknown flag fails loud instead of being ignored", () => {
    const r = normalizeMcpConfigInput(`qodercli mcp add --oauth-pkce srv https://x/mcp`, "");
    expect(r.errors[0]?.code).toBe("cli_parse");
    expect(r.errors[0]?.message).toContain("--oauth-pkce");
  });

  it("name-less command falls back to the dialog name field", () => {
    const r = normalizeMcpConfigInput(`npx -y @some/mcp-server`, "my-name");
    expect(r.servers[0].name).toBe("my-name");
  });

  it("http transport without any url fails with invalid_combination", () => {
    const r = normalizeMcpConfigInput(`qodercli mcp add --transport http only-name`, "");
    expect(r.errors[0]?.code).toBe("invalid_combination");
  });
});

describe("shellTokenize", () => {
  it("handles mixed quotes and escapes", () => {
    expect(shellTokenize(`a 'b c' "d \\"e" f\\ g`)).toEqual(["a", "b c", 'd "e', "f g"]);
  });
});
