/**
 * Unverified JWT payload readers (see `src/lib/jwt.ts`).
 *
 * These decide *when to rotate* the access token, never whether a token is
 * trusted — the Gateway verifies it. So the tests pin the decoding rules
 * (base64url, un-padded, UTF-8) rather than any security property.
 */
import { describe, it, expect } from "vitest";

import { decodeJwtPayload, jwtExpMs } from "./jwt";

/** base64url of a UTF-8 string, no padding — the JWT segment shape. */
function b64url(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (const b of bytes) binary += String.fromCharCode(b);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

const token = (payload: string) => `${b64url('{"alg":"EdDSA"}')}.${b64url(payload)}.sig`;

describe("decodeJwtPayload", () => {
  it("decodes a standard payload", () => {
    expect(decodeJwtPayload(token('{"sub":"u-1","exp":1900}'))).toEqual({
      sub: "u-1",
      exp: 1900,
    });
  });

  it("survives non-ASCII claims", () => {
    expect(decodeJwtPayload(token('{"name":"张三"}'))).toEqual({ name: "张三" });
  });

  it("returns null for structural garbage and non-object payloads", () => {
    expect(decodeJwtPayload("not-a-token")).toBeNull();
    expect(decodeJwtPayload("onlyonesegment")).toBeNull();
    expect(decodeJwtPayload(`${b64url('{"a":1}')}.`)).toBeNull();
    expect(decodeJwtPayload(token("42"))).toBeNull();
    expect(decodeJwtPayload(token('"a string"'))).toBeNull();
    expect(decodeJwtPayload(token("not json"))).toBeNull();
  });
});

describe("jwtExpMs", () => {
  it("converts the seconds-based exp claim to milliseconds", () => {
    expect(jwtExpMs(token('{"sub":"u-1","exp":1900}'))).toBe(1_900_000);
  });

  it("returns null when exp is missing or not a finite number", () => {
    expect(jwtExpMs(token('{"sub":"u-1"}'))).toBeNull();
    expect(jwtExpMs(token('{"exp":"1900"}'))).toBeNull();
    expect(jwtExpMs(token('{"exp":null}'))).toBeNull();
    expect(jwtExpMs("not-a-token")).toBeNull();
  });
});
