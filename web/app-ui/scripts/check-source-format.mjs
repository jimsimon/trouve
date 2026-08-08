import { readFile, readdir } from "node:fs/promises";
import { extname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const extensions = new Set([".css", ".html", ".json", ".mjs", ".ts"]);
const roots = ["e2e", "scripts", "src"];
const topLevel = [
  "gallery.html",
  "index.html",
  "package.json",
  "playwright.config.ts",
  "tsconfig.json",
  "tsconfig.worker.json",
  "vite.config.ts",
];

const files = [];
const visit = async (relative) => {
  const entries = await readdir(join(root, relative), { withFileTypes: true });
  for (const entry of entries) {
    const child = join(relative, entry.name);
    if (entry.isDirectory()) await visit(child);
    else if (extensions.has(extname(entry.name))) files.push(child);
  }
};
for (const directory of roots) await visit(directory);
files.push(...topLevel);
files.sort();

const errors = [];
for (const relative of files) {
  const source = await readFile(join(root, relative), "utf8");
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
