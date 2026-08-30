import { describe, expect, it } from "vitest";

import { jsonNumberTokenIsExact, parseProtocolJson } from "./protocol-json.js";

describe("protocol JSON number preservation", () => {
  it("recognizes equivalent tokens without accepting rounded values", () => {
    expect(jsonNumberTokenIsExact("1e20", 1e20)).toBe(true);
    expect(jsonNumberTokenIsExact("0.10000000000000000", 0.1)).toBe(true);
    expect(jsonNumberTokenIsExact("9007199254740993", 9_007_199_254_740_992)).toBe(false);
    expect(jsonNumberTokenIsExact("0.1234567890123456789", 0.12345678901234568))
      .toBe(false);
  });

  it("hides lossy schema properties and drops lossy stored options", () => {
    expect(parseProtocolJson(`{
      "cursor": 9007199254740993,
      "options_schema": {
        "type": "object",
        "properties": {
          "safe": {"type": "number", "default": 1e20},
          "unsafe_integer": {"type": "number", "default": 9007199254740993},
          "unsafe_decimal": {"type": "number", "minimum": 0.1234567890123456789}
        }
      },
      "model_options": {
        "safe": 1e20,
        "unsafe_integer": 9007199254740993,
        "unsafe_decimal": 0.1234567890123456789
      }
    }`)).toEqual({
      cursor: 9_007_199_254_740_992,
      options_schema: {
        type: "object",
        properties: {
          safe: { type: "number", default: 1e20 },
        },
      },
      model_options: { safe: 1e20 },
    });
  });
});
