import { describe, expect, it } from "vitest";

import { buildTodoPlanModel } from "./todo-plan-model.js";

describe("buildTodoPlanModel", () => {
  it("preserves authoritative order and represents every semantic status", () => {
    const model = buildTodoPlanModel([
      { id: "pending", content: "Queue work", status: "pending" },
      { id: "active", content: "Build surface", status: "in_progress" },
      { id: "done", content: "Audit Slint", status: "completed" },
      { id: "cancelled", content: "Discard spike", status: "cancelled" },
    ]);

    expect(model.rows.map(({ id }) => id)).toEqual([
      "pending",
      "active",
      "done",
      "cancelled",
    ]);
    expect(model.rows.map(({ icon, statusLabel }) => ({ icon, statusLabel }))).toEqual([
      { icon: "circle", statusLabel: "Pending" },
      { icon: "play", statusLabel: "In progress" },
      { icon: "check", statusLabel: "Completed" },
      { icon: "xmark", statusLabel: "Cancelled" },
    ]);
    expect(model.current?.id).toBe("active");
    expect(model.rows.filter(({ current }) => current).map(({ id }) => id)).toEqual([
      "active",
    ]);
  });

  it("matches desktop progress semantics, including cancelled items in total", () => {
    const model = buildTodoPlanModel([
      { id: "done", content: "Done", status: "completed" },
      { id: "cancelled", content: "Cancelled", status: "cancelled" },
      { id: "next", content: "Next", status: "pending" },
    ]);

    expect(model).toMatchObject({
      total: 3,
      completed: 1,
      pending: 1,
      inProgress: 0,
      cancelled: 1,
      progressLabel: "1/3 complete",
      progressPercent: 33,
      current: undefined,
    });
  });

  it("provides a stable empty projection", () => {
    expect(buildTodoPlanModel([])).toMatchObject({
      rows: [],
      total: 0,
      completed: 0,
      progressLabel: "0/0 complete",
      progressPercent: 0,
      current: undefined,
    });
  });
});
