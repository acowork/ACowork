import { describe, expect, it } from "vitest";
import { OFFLINE_JSON_EXAMPLE, offlineSpecToModel, parseOfflineJson } from "./offlineJson";

describe("offlineSpecToModel", () => {
  it("maps an offline spec into ModelInfo + capabilities", () => {
    const r = offlineSpecToModel(
      {
        id: "lynkr-auto",
        name: "Lynkr Auto",
        attachment: false,
        reasoning: false,
        tool_call: true,
        temperature: true,
        modalities: { input: ["text", "image"], output: ["text"] },
        limit: { context: 128000, output: 8192 },
        cost: { input: 0, output: 0 },
        family: "auto",
        knowledge: "2025-12",
      },
      "lynkr-auto",
    );
    expect(r).not.toBeNull();
    expect(r!.info).toMatchObject({
      id: "lynkr-auto",
      name: "Lynkr Auto",
      family: "auto",
      tool_call: true,
      context_window: 128000,
      max_tokens: 8192,
      input_modalities: ["text", "image"],
      output_modalities: ["text"],
      knowledge: "2025-12",
    });
    expect(r!.cap).toMatchObject({
      context_window: 128000,
      max_output_tokens: 8192,
      supports_tool_calling: true,
      supports_reasoning: false,
      modalities: { input: ["text", "image"], output: ["text"] },
    });
  });

  it("falls back to the map key when spec.id is missing", () => {
    const r = offlineSpecToModel({ name: "Foo" }, "foo-key");
    expect(r?.info.id).toBe("foo-key");
    expect(r?.info.name).toBe("Foo");
  });

  it("returns null when no usable id is available", () => {
    expect(offlineSpecToModel({}, "")).toBeNull();
  });

  it("uses sensible defaults when limit/modalities are missing", () => {
    const r = offlineSpecToModel({ id: "x", name: "X" }, "x");
    expect(r!.cap.context_window).toBe(128000);
    expect(r!.cap.max_output_tokens).toBe(16384);
    expect(r!.cap.modalities).toEqual({ input: ["text"], output: ["text"] });
  });
});

describe("parseOfflineJson", () => {
  it("accepts a full offline_providers.json provider entry", () => {
    const raw = JSON.stringify({
      id: "lynkr",
      env: ["LYNKR_API_KEY"],
      npm: "@ai-sdk/openai-compatible",
      api: "http://127.0.0.1:8081/v1",
      name: "Lynkr",
      models: {
        "lynkr-auto": {
          id: "lynkr-auto",
          name: "Lynkr Auto",
          tool_call: true,
          modalities: { input: ["text"], output: ["text"] },
          limit: { context: 128000, output: 8192 },
        },
      },
    });
    const r = parseOfflineJson(raw);
    expect(r?.models).toHaveLength(1);
    expect(r?.models[0].id).toBe("lynkr-auto");
    expect(r?.caps["lynkr-auto"].context_window).toBe(128000);
  });

  it("accepts a bare id→spec map (key = model id)", () => {
    const raw = JSON.stringify({
      "gpt-x": { name: "GPT X", tool_call: true, limit: { context: 8000, output: 4000 } },
      "gpt-y": { name: "GPT Y", tool_call: false },
    });
    const r = parseOfflineJson(raw);
    expect(r?.models.map((m) => m.id)).toEqual(["gpt-x", "gpt-y"]);
    expect(r?.caps["gpt-x"].max_output_tokens).toBe(4000);
  });

  it("accepts a single model spec", () => {
    const raw = JSON.stringify({
      id: "solo",
      name: "Solo",
      tool_call: true,
      limit: { context: 32000, output: 4000 },
    });
    const r = parseOfflineJson(raw);
    expect(r?.models).toHaveLength(1);
    expect(r?.models[0].id).toBe("solo");
  });

  it("rejects invalid JSON", () => {
    expect(parseOfflineJson("{ not json")).toBeNull();
  });

  it("rejects shapes that don't look like any of the three", () => {
    expect(parseOfflineJson(JSON.stringify([1, 2, 3]))).toBeNull();
    expect(parseOfflineJson(JSON.stringify(42))).toBeNull();
    expect(parseOfflineJson(JSON.stringify("hello"))).toBeNull();
  });

  it("accepts a pre-parsed object (not just a string)", () => {
    const r = parseOfflineJson({ id: "x", name: "X", tool_call: true });
    expect(r?.models[0].id).toBe("x");
  });

  it("the exported example is valid JSON and parses to two models", () => {
    // Sanity-check the demo we show to users in the dialog.
    expect(() => JSON.parse(OFFLINE_JSON_EXAMPLE)).not.toThrow();
    const r = parseOfflineJson(OFFLINE_JSON_EXAMPLE);
    expect(r?.models.map((m) => m.id)).toEqual(["my-model-large", "my-model-small"]);
    expect(r?.caps["my-model-large"].context_window).toBe(128000);
    expect(r?.caps["my-model-large"].modalities?.input).toEqual(["text", "image"]);
    expect(r?.caps["my-model-small"].max_output_tokens).toBe(4000);
  });
});