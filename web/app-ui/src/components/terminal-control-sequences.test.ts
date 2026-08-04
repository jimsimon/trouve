import { describe, expect, it } from "vitest";

import {
  normalizeTerminalTitle,
  terminalRequestedSize,
} from "./terminal-control-sequences.js";

describe("terminal control sequences", () => {
  it("bounds terminal titles by UTF-8 bytes and strips control characters", () => {
    expect(normalizeTerminalTitle("dev\nserver\u0000")).toBe("devserver");
    expect(normalizeTerminalTitle(`${"a".repeat(511)}é`)).toBe(
      `${"a".repeat(511)}�`,
    );
    expect(normalizeTerminalTitle("\u0000\n\u007f")).toBe("");
  });

  it("recognizes only positive CSI window resize requests", () => {
    expect(terminalRequestedSize([8, 42, 132])).toEqual({ cols: 132, rows: 42 });
    expect(terminalRequestedSize([3, 42, 132])).toBeUndefined();
    expect(terminalRequestedSize([8, 0, 132])).toBeUndefined();
    expect(terminalRequestedSize([8, [42], 132])).toBeUndefined();
  });
});
