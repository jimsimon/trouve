import { describe, expect, it } from "vitest";

import {
  decodeOsc52ClipboardRequest,
  parseOsc52ClipboardRequest,
} from "./terminal-clipboard.js";

describe("OSC 52 clipboard requests", () => {
  it("decodes bounded UTF-8 clipboard text", () => {
    const payload = btoa(String.fromCharCode(...new TextEncoder().encode("hello 🌍")));
    expect(decodeOsc52ClipboardRequest(`c;${payload}`)).toBe("hello 🌍");
    expect(parseOsc52ClipboardRequest(`c;${payload}`)).toEqual({
      kind: "copy",
      text: "hello 🌍",
    });
    expect(parseOsc52ClipboardRequest(`p;${payload}`)).toEqual({
      kind: "copy",
      text: "hello 🌍",
    });
    expect(parseOsc52ClipboardRequest(`;${payload}`)).toEqual({
      kind: "copy",
      text: "hello 🌍",
    });
  });

  it("rejects clipboard queries, malformed targets, base64, and UTF-8", () => {
    expect(decodeOsc52ClipboardRequest("c;?")).toBeUndefined();
    expect(decodeOsc52ClipboardRequest("evil;aGVsbG8=")).toBeUndefined();
    expect(decodeOsc52ClipboardRequest("c;***")).toBeUndefined();
    expect(decodeOsc52ClipboardRequest("c;/w==")).toBeUndefined();
    expect(parseOsc52ClipboardRequest("c;?")).toEqual({ kind: "read" });
    expect(parseOsc52ClipboardRequest("evil;aGVsbG8=")).toEqual({
      kind: "invalid",
    });
  });

  it("rejects oversized requests before decoding", () => {
    expect(decodeOsc52ClipboardRequest(`c;${"A".repeat(256 * 1024 + 1)}`)).toBeUndefined();
  });
});
