import { describe, expect, it } from "vitest";

import { routingReasonLabel } from "./routing-labels.js";

describe("routing reason labels", () => {
  it("baseline routing labels follow the durable job mode", () => {
    expect(routingReasonLabel("baseline", "additive")).toBe("Additive baseline");
    expect(routingReasonLabel("baseline", "automatic")).toBe("Automatic baseline");
    expect(routingReasonLabel("baseline", "manual")).toBe("Routing baseline");
  });

  it("unknown routing sources remain visible", () => {
    expect(routingReasonLabel("future-router", "automatic")).toBe("future-router");
  });

  it("historical deterministic routing remains recognizable", () => {
    expect(routingReasonLabel("deterministic", "additive")).toBe("Legacy diff signal");
  });
});
