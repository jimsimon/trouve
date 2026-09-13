import { readFile, readdir } from "node:fs/promises";
import { join } from "node:path";

import { workspaceRoot, workspaceSources } from "./workspace-sources.mjs";

const extensions = new Set([".css", ".html", ".json", ".mjs", ".ts"]);
const files = await workspaceSources({
  directories: ["e2e", "scripts", "src"],
  topLevel: [
    "gallery.html",
    "index.html",
    "package.json",
    "playwright.config.ts",
    "tsconfig.json",
    "tsconfig.worker.json",
    "vite.config.ts",
  ],
  extensions,
});
files.push("package.json", "tsconfig.base.json");
for (const entry of await readdir(join(workspaceRoot, "scripts"))) {
  if (entry.endsWith(".mjs")) files.push(join("scripts", entry));
}
files.sort();

const errors = [];
for (const relative of files) {
  const source = await readFile(join(workspaceRoot, relative), "utf8");
  if (source.includes("\r")) errors.push(`${relative}: use LF line endings`);
  if (!source.endsWith("\n")) errors.push(`${relative}: add a final newline`);
  source.split("\n").forEach((line, index) => {
    if (/[ \t]+$/u.test(line)) {
      errors.push(`${relative}:${index + 1}: remove trailing whitespace`);
    }
    if (line.includes("\t")) {
      errors.push(`${relative}:${index + 1}: use spaces instead of tabs`);
    }
  });
}

if (errors.length > 0) {
  throw new Error(`source formatting check failed:\n${errors.join("\n")}`);
}
console.log(`source formatting hygiene passed (${files.length} files)`);
