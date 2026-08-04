import { readFile, readdir } from "node:fs/promises";
import { extname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const files = [];
const visit = async (relative) => {
  const entries = await readdir(join(root, relative), { withFileTypes: true });
  for (const entry of entries) {
    const child = join(relative, entry.name);
    if (entry.isDirectory()) await visit(child);
    else if ([".mjs", ".ts"].includes(extname(entry.name))) files.push(child);
  }
};
for (const directory of ["e2e", "src"]) await visit(directory);
files.push("playwright.config.ts", "vite.config.ts");
files.sort();

const policies = [
  {
    expression: /\b(?:eval|Function)\s*\(/u,
    message: "dynamic code execution violates the desktop CSP",
  },
  {
    expression: /\bnew\s+Function\s*\(/u,
    message: "dynamic code execution violates the desktop CSP",
  },
  {
    expression: /(?:\.innerHTML\s*=|insertAdjacentHTML\s*\(|document\.write\s*\()/u,
    message: "raw HTML sinks bypass Lit and the Markdown sanitizer",
  },
  {
    expression: /(?:from\s+|import\s*)["'`](?:preact|react)(?:\/[^"'`]*)?["'`]/u,
    message: "Lit is the only application component runtime",
  },
  {
    expression: /import\s*(?:\([^)]*)?["'`]https?:\/\//u,
    message: "runtime modules must be bundled and self-hosted",
  },
  {
    expression: /webawesome-pro/iu,
    message: "WebAwesome Pro is outside the approved MIT dependency policy",
  },
];

const errors = [];
for (const relative of files) {
  const source = await readFile(join(root, relative), "utf8");
  for (const { expression, message } of policies) {
    if (expression.test(source)) errors.push(`${relative}: ${message}`);
  }
}

if (errors.length > 0) {
  throw new Error(`source policy lint failed:\n${errors.join("\n")}`);
}
console.log(`source policy lint passed (${files.length} files)`);
