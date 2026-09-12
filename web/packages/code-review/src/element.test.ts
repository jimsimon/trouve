import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import { Effect, LatestRequest } from "./element.js";

const read = (file: string): string => readFileSync(new URL(file, import.meta.url), "utf8");

interface Deferred<T> {
  readonly promise: Promise<T>;
  resolve(value: T): void;
  reject(cause: unknown): void;
}

const deferred = <T>(): Deferred<T> => {
  let resolve!: (value: T) => void;
  let reject!: (cause: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
};

describe("Effect ownership", () => {
  it("marks a run stale once its deps change or it is disposed", () => {
    const effect = new Effect();
    const runs: Array<() => boolean> = [];
    effect.run(["a"], (isCurrent) => {
      runs.push(isCurrent);
    });
    effect.run(["a"], (isCurrent) => {
      runs.push(isCurrent);
    });
    expect(runs).toHaveLength(1);
    expect(runs[0]?.()).toBe(true);

    effect.run(["b"], (isCurrent) => {
      runs.push(isCurrent);
    });
    expect(runs).toHaveLength(2);
    expect(runs[0]?.()).toBe(false);
    expect(runs[1]?.()).toBe(true);

    effect.dispose();
    expect(runs[1]?.()).toBe(false);
  });

  it("runs the previous cleanup before the replacement body sees ownership", () => {
    const effect = new Effect();
    const order: string[] = [];
    effect.run([1], () => () => order.push("cleanup 1"));
    effect.run([2], (isCurrent) => {
      order.push(`run 2 current=${isCurrent()}`);
    });
    expect(order).toEqual(["cleanup 1", "run 2 current=true"]);
  });
});

describe("LatestRequest", () => {
  it("lets only the newest request publish, whatever order responses arrive in", async () => {
    // Mirrors the jobs page: filter A is requested, then filter B; B answers
    // first and A's late answer (or error) must not replace it.
    const requests = new LatestRequest();
    let jobs = "initial";
    let error = "";
    const load = async (filter: string, response: Promise<string>): Promise<void> => {
      const isCurrent = requests.begin();
      try {
        const next = await response;
        if (!isCurrent()) return;
        jobs = next;
        error = "";
      } catch (cause) {
        if (!isCurrent()) return;
        error = String(cause);
      }
    };
    const a = deferred<string>();
    const b = deferred<string>();
    const loadA = load("A", a.promise);
    const loadB = load("B", b.promise);

    b.resolve("jobs for B");
    await loadB;
    expect(jobs).toBe("jobs for B");

    a.resolve("jobs for A");
    await loadA;
    expect(jobs).toBe("jobs for B");

    const c = deferred<string>();
    const d = deferred<string>();
    const loadC = load("C", c.promise);
    const loadD = load("D", d.promise);
    d.resolve("jobs for D");
    await loadD;
    c.reject(new Error("C failed late"));
    await loadC;
    expect(jobs).toBe("jobs for D");
    expect(error).toBe("");
  });
});

describe("stale asynchronous results in the review elements", () => {
  it("the jobs page publishes only the latest filter's response", () => {
    const source = read("./jobs-page.ts");
    expect(source).toMatch(/jobsRequest = new LatestRequest\(\)/u);
    expect(source).toMatch(
      /const isCurrent = this\.jobsRequest\.begin\(\);\s*try \{\s*const \{ jobs \} = await this\.api\.getJobs\(status, repository\);\s*if \(!isCurrent\(\)\) return;\s*this\.jobs = jobs;/u,
    );
    expect(source).toMatch(/catch \(cause\) \{\s*if \(!isCurrent\(\)\) return;\s*this\.error = errorMessage\(cause\);/u);
  });

  it("login polls merge into the still-current pending login only", () => {
    const source = read("./provider-settings-card.ts");
    const start = source.indexOf("this.loginPollEffect.run(");
    const poll = source.slice(start, source.indexOf("const { subscriptionProviders }", start));
    expect(poll).toMatch(/this\.loginPollEffect\.run\(\[login\?\.provider\.id, login\?\.status\], \(isCurrent\) =>/u);
    // Ownership: the effect is live, the same provider, and still pending.
    expect(poll).toMatch(
      /isCurrent\(\)\s*&& current !== null\s*&& current\.provider\.id === login\.provider\.id\s*&& current\.status === "pending"/u,
    );
    // Success, failure, and transport errors all spread the current login
    // (keeping a submitted code) rather than the one captured at poll start.
    expect(poll).toMatch(/this\.login = \{ \.\.\.current, status: "success", error: "" \};/u);
    expect(poll).toMatch(/this\.login = \{ \.\.\.current, status: "failed", error: state\.error \|\| "Sign-in failed" \};/u);
    expect(poll).toMatch(/catch \(cause\) \{\s*const current = currentPendingLogin\(\);\s*if \(!current\) return;\s*this\.login = \{ \.\.\.current, error: errorMessage\(cause\) \};/u);
    expect(poll).not.toMatch(/\.\.\.login,/u);
  });
});
