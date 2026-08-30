import { describe, expect, it, vi } from "vitest";

import {
  jsonNumberTokenIsExact,
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
      expect(() => parseProtocolJson('{"model_options":{"temperature":0.25}}'))
        .toThrow(UnsupportedModelOptionNumberError);
    } finally {
      parseWithoutSource.mockRestore();
    }
  });
});
