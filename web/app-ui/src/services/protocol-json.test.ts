import { describe, expect, it, vi } from "vitest";

import {
  jsonNumberTokenIsExact,
  modelOptionNumberNeedsSourceProof,
  parseProtocolJson,
  stringifyProtocolJson,
  UnsupportedModelOptionNumberError,
} from "./protocol-json.js";

describe("protocol JSON number preservation", () => {
  it("recognizes equivalent tokens without accepting rounded values", () => {
    expect(jsonNumberTokenIsExact("1e20", 1e20)).toBe(true);
    expect(jsonNumberTokenIsExact("0.10000000000000000", 0.1)).toBe(true);
    expect(jsonNumberTokenIsExact("9007199254740993", 9_007_199_254_740_992)).toBe(false);
    expect(jsonNumberTokenIsExact("0.1234567890123456789", 0.12345678901234568))
      .toBe(false);
    expect(jsonNumberTokenIsExact("-0", -0)).toBe(true);
    expect(jsonNumberTokenIsExact("-0", 0)).toBe(false);
    expect(jsonNumberTokenIsExact("01", 1)).toBe(false);
  });

  it("requires source proof for decimals as well as unsafe integers", () => {
    expect(modelOptionNumberNeedsSourceProof(1)).toBe(false);
    expect(modelOptionNumberNeedsSourceProof(Number.MAX_SAFE_INTEGER)).toBe(false);
    expect(modelOptionNumberNeedsSourceProof(0.5)).toBe(true);
    expect(modelOptionNumberNeedsSourceProof(0.12345678901234568)).toBe(true);
    expect(modelOptionNumberNeedsSourceProof(Number.MAX_SAFE_INTEGER + 1)).toBe(true);
    expect(modelOptionNumberNeedsSourceProof(-0)).toBe(true);
  });

  it("hides lossy schema properties without rounding ordinary protocol numbers", () => {
    expect(parseProtocolJson(`{
      "cursor": 9007199254740993,
      "options_schema": {
        "type": "object",
        "properties": {
          "safe": {"type": "number", "default": 1e20},
          "unsafe_integer": {"type": "number", "default": 9007199254740993},
          "unsafe_decimal": {"type": "number", "minimum": 0.1234567890123456789}
        }
      }
    }`)).toEqual({
      cursor: 9_007_199_254_740_992,
      options_schema: {
        type: "object",
        properties: {
          safe: { type: "number", default: 1e20 },
        },
      },
    });
  });

  it("rejects lossy persisted options instead of deleting them from later saves", () => {
    expect(() => parseProtocolJson(
      '{"model_options":{"temperature":0.1234567890123456789}}',
    )).toThrow(UnsupportedModelOptionNumberError);
    const exact = parseProtocolJson(
      '{"model_options":{"temperature":0.10000000000000000,"large":1e20,"zero":-0}}',
    );
    expect(exact).toEqual({ model_options: { temperature: 0.1, large: 1e20, zero: -0 } });
    expect(stringifyProtocolJson(exact)).toBe(
      '{"model_options":{"temperature":0.10000000000000000,"large":1e20,"zero":-0}}',
    );
    expect(() => stringifyProtocolJson({
      model_options: { temperature: 0.12345678901234568 },
    })).toThrow(UnsupportedModelOptionNumberError);

    const stale = parseProtocolJson(
      '{"model_options":{"temperature":0.10000000000000000}}',
    ) as { model_options: { temperature: number } };
    stale.model_options.temperature = 0.12345678901234568;
    expect(() => stringifyProtocolJson(stale)).toThrow(UnsupportedModelOptionNumberError);

    const nativeParse = JSON.parse.bind(JSON);
    const parseWithoutSource = vi.spyOn(JSON, "parse").mockImplementation((text, reviver) =>
      nativeParse(text, reviver === undefined ? undefined : function (key, value) {
        return reviver.call(this, key, value);
      })
    );
    try {
      const fallbackSource = "{\"model_options\":{\"temperature\":0.2500,\"large\":1e20,\"zero\":-0},\"label\":\"0.2500\"}";
      const fallback = parseProtocolJson(fallbackSource);
      expect(fallback).toEqual({
        model_options: { temperature: 0.25, large: 1e20, zero: -0 },
        label: "0.2500",
      });
      expect(stringifyProtocolJson(fallback)).toBe(fallbackSource);
      expect(() => parseProtocolJson(
        '{"model_options":{"temperature":0.1234567890123456789}}',
      )).toThrow(UnsupportedModelOptionNumberError);
      expect(parseProtocolJson(`{
        "options_schema":{"properties":{
          "safe":{"type":"number","default":0.25},
          "unsafe":{"type":"number","default":9007199254740993}
        }}
      }`)).toEqual({
        options_schema: { properties: { safe: { type: "number", default: 0.25 } } },
      });
    } finally {
      parseWithoutSource.mockRestore();
    }
  });
});
