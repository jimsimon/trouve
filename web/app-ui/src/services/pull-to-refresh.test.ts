import { describe, expect, it } from "vitest";

import { PullToRefreshGesture } from "./pull-to-refresh.js";

describe("PullToRefreshGesture", () => {
  it("starts only at the scroll boundary and applies resistance", () => {
    const gesture = new PullToRefreshGesture(60, 90);
    expect(gesture.begin(10, 20, false)).toBe(false);
    expect(gesture.move(10, 200)).toEqual({ distance: 0, armed: false });

    expect(gesture.begin(10, 20, true)).toBe(true);
    expect(gesture.move(12, 100)).toEqual({ distance: 44, armed: false });
    expect(gesture.move(12, 220)).toEqual({ distance: 90, armed: true });
    expect(gesture.finish()).toBe(true);
    expect(gesture.state).toEqual({ distance: 0, armed: false });
  });

  it("cancels upward and horizontal gestures", () => {
    const gesture = new PullToRefreshGesture();
    gesture.begin(50, 50, true);
    expect(gesture.move(90, 60)).toEqual({ distance: 0, armed: false });
    expect(gesture.finish()).toBe(false);

    gesture.begin(50, 50, true);
    expect(gesture.move(50, 40)).toEqual({ distance: 0, armed: false });
  });
});
