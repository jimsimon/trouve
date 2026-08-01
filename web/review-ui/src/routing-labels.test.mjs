import assert from "node:assert/strict";
import test from "node:test";

import { routingReasonLabel } from "./routing-labels.ts";

test("baseline routing labels follow the durable job mode", () => {
  assert.equal(routingReasonLabel("baseline", "additive"), "Additive baseline");
  assert.equal(routingReasonLabel("baseline", "automatic"), "Automatic baseline");
  assert.equal(routingReasonLabel("baseline", "manual"), "Routing baseline");
});

test("unknown routing sources remain visible", () => {
  assert.equal(routingReasonLabel("future-router", "automatic"), "future-router");
});
