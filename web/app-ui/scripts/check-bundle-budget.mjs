import { readdirSync, readFileSync, statSync } from "node:fs";
import { basename, resolve } from "node:path";

const mode = process.argv[2];
if (mode !== "desktop" && mode !== "pwa") {
  throw new Error("usage: node scripts/check-bundle-budget.mjs <desktop|pwa>");
}

const root = resolve("dist", mode);
const assets = resolve(root, "assets");
const files = readdirSync(assets).map((name) => ({
  name,
  bytes: statSync(resolve(assets, name)).size,
}));
const javascript = files.filter(({ name }) => name.endsWith(".js"));
const styles = files.filter(({ name }) => name.endsWith(".css"));
const fonts = files.filter(({ name }) => name.endsWith(".woff2"));
const index = readFileSync(resolve(root, "index.html"), "utf8");
const entryName = /assets\/(app-[A-Za-z0-9_-]+\.js)/u.exec(index)?.[1];
const entry = javascript.find(({ name }) => name === entryName);
const worker = javascript.find(({ name }) => name.startsWith("content-worker-"));

// Desktop retains the native-capability adapter in its entry chunk. Keep its
// allowance explicit and narrowly above the durable compaction, chat-
// presentation preference, and Font Awesome icon UI; the PWA remains on the
// original entry ceiling. Font assets have their own explicit budget below.
const entryLimit = mode === "desktop" ? 856_000 : 850_000;
// The locked Vite/Rolldown graph emits 3,188,254 B after the attachment
// gallery and external-video handoff. Keep about 4 kB of headroom above that
// artifact; entry, worker, and largest-chunk budgets below still prevent one
// bundle from hiding in the aggregate.
const totalJavaScriptLimit = 3_193_000;
// The version-check UI brings the clean baseline to 180,450 B of CSS. Keep
// less than 2 kB of headroom while retaining a regression-sensitive ceiling.
const totalStyleLimit = 182_000;
const limits = {
  entry: entryLimit,
  worker: 350_000,
  javascript: totalJavaScriptLimit,
  styles: totalStyleLimit,
  fonts: 125_000,
  largestChunk: entryLimit,
};
const total = (entries) => entries.reduce((bytes, entry) => bytes + entry.bytes, 0);
const fail = (message) => {
  throw new Error(`${mode} bundle budget exceeded: ${message}`);
};

if (entry === undefined) fail("the hashed application entry could not be identified");
if (entry.bytes > limits.entry) fail(`${entry.name} is ${entry.bytes} bytes (limit ${limits.entry})`);
if (worker === undefined) fail("the lazy content worker is missing");
if (worker.bytes > limits.worker) fail(`${worker.name} is ${worker.bytes} bytes (limit ${limits.worker})`);
const largest = javascript.reduce((current, entry) =>
  entry.bytes > current.bytes ? entry : current, { name: "", bytes: 0 });
if (largest.bytes > limits.largestChunk) {
  fail(`${largest.name} is ${largest.bytes} bytes (chunk limit ${limits.largestChunk})`);
}
if (total(javascript) > limits.javascript) {
  fail(`JavaScript totals ${total(javascript)} bytes (limit ${limits.javascript})`);
}
if (total(styles) > limits.styles) {
  fail(`CSS totals ${total(styles)} bytes (limit ${limits.styles})`);
}
if (total(fonts) > limits.fonts) {
  fail(`font assets total ${total(fonts)} bytes (limit ${limits.fonts})`);
}

console.log(
  `${mode} bundle within budget: entry ${basename(entry.name)} ${entry.bytes} B, `
  + `worker ${worker.bytes} B, JS ${total(javascript)} B, CSS ${total(styles)} B, `
  + `fonts ${total(fonts)} B`,
);
