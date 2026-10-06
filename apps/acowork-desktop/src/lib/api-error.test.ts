import { describe, expect, it } from "vitest";
import { friendlyErrorMessage, httpApiError, NOT_AUTHORIZED } from "./api-error";

/** Minimal Response stand-in — httpApiError only touches status/json(). */
function res(status: number, body: unknown): Response {
  return {
    status,
    json: () => Promise.resolve(body),
  } as unknown as Response;
}

describe("httpApiError", () => {
  it("maps the flat Gateway denial to a tier-refined permission message", async () => {
    const err = await httpApiError(
      res(403, {
        error: "forbidden",
        code: NOT_AUTHORIZED,
        resource: "agent",
        required: "manage",
      }),
    );
    expect(err.status).toBe(403);
    expect(err.code).toBe(NOT_AUTHORIZED);
    expect(err.required).toBe("manage");
    // Localized copy, not a bare "HTTP 403".
    expect(err.message).not.toMatch(/HTTP 403/);
    expect(err.message.length).toBeGreaterThan(0);
  });

  it("maps the nested doc-service denial as well", async () => {
    const err = await httpApiError(
      res(403, { error: { code: NOT_AUTHORIZED, message: "forbidden: …" } }),
    );
    expect(err.code).toBe(NOT_AUTHORIZED);
    expect(err.message).not.toMatch(/HTTP 403/);
  });

  it("keeps the runtime error string for non-permission failures", async () => {
    const err = await httpApiError(res(409, { error: "distiller is disabled" }));
    expect(err.message).toBe("distiller is disabled");
  });

  it("falls back to HTTP <status> on a non-JSON body", async () => {
    const r = { status: 502, json: () => Promise.reject(new Error("no json")) };
    const err = await httpApiError(r as unknown as Response);
    expect(err.message).toBe("HTTP 502");
  });
});

describe("friendlyErrorMessage", () => {
  it("duck-types DocApiError-shaped denials (no instanceof import cycle)", () => {
    const docErr = Object.assign(new Error("whatever"), {
      name: "DocApiError",
      status: 403,
      code: NOT_AUTHORIZED,
      required: "use",
    });
    expect(friendlyErrorMessage(docErr, "fallback")).not.toBe("whatever");
    expect(friendlyErrorMessage(docErr, "fallback").length).toBeGreaterThan(0);
  });

  it("passes ordinary errors through", () => {
    expect(friendlyErrorMessage(new Error("boom"), "fb")).toBe("boom");
    expect(friendlyErrorMessage("x", "fb")).toBe("fb");
  });
});
