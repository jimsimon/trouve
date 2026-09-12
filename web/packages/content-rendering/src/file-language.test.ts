import { describe, expect, it } from "vitest";

import { languageForPath } from "./file-language.js";

describe("file preview language inference", () => {
  it("covers representative native syntect file types", () => {
    expect(languageForPath("src/main.rs")).toBe("rust");
    expect(languageForPath("tools/release.py")).toBe("python");
    expect(languageForPath("scripts/check.sh")).toBe("shell");
    expect(languageForPath("config/settings.yaml")).toBe("yaml");
    expect(languageForPath("Cargo.toml")).toBe("toml");
    expect(languageForPath("Dockerfile")).toBe("dockerfile");
    expect(languageForPath("src/main.tsx")).toBe("tsx");
    expect(languageForPath("assets/data.unknown")).toBe("text");
  });
});
