import { readFile } from "node:fs/promises";
import { join } from "node:path";

import { workspaceMembers, workspaceRoot, workspaceSources } from "./workspace-sources.mjs";

const files = await workspaceSources({
  directories: ["e2e", "src"],
  topLevel: ["playwright.config.ts", "vite.config.ts"],
  extensions: new Set([".mjs", ".ts"]),
});

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
    expression: /(?:from\s+|import\s*)["'`](?:preact|react(?:-dom)?)(?:\/[^"'`]*)?["'`]/u,
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

// Every `@trouve-ai/*` workspace package a member imports must be declared in
// that member's own manifest. Root-workspace hoisting resolves an undeclared
// sibling anyway, which would hide the omission until a package-scoped
// install, a Docker build that stages manifests selectively, or a dependency
// audit that walks the declared closure.
const manifests = new Map();
for (const member of await workspaceMembers()) {
  const manifest = JSON.parse(await readFile(join(workspaceRoot, member, "package.json"), "utf8"));
  manifests.set(member, {
    name: manifest.name,
    declared: new Set([
      ...Object.keys(manifest.dependencies ?? {}),
      ...Object.keys(manifest.devDependencies ?? {}),
      ...Object.keys(manifest.peerDependencies ?? {}),
    ]),
  });
}
const workspaceImport = /(?:from\s+|import\s*\(?\s*)["'`](@trouve-ai\/[^/"'`]+)/gu;
const memberOf = (relative) =>
  [...manifests.keys()].find((member) => relative.startsWith(`${member}/`));

const errors = [];
for (const relative of files) {
  const source = await readFile(join(workspaceRoot, relative), "utf8");
  for (const { expression, message } of policies) {
    if (expression.test(source)) errors.push(`${relative}: ${message}`);
  }
  const manifest = manifests.get(memberOf(relative));
  if (manifest === undefined) continue;
  const undeclared = new Set();
  for (const [, specifier] of source.matchAll(workspaceImport)) {
    if (specifier !== manifest.name && !manifest.declared.has(specifier)) undeclared.add(specifier);
  }
  for (const specifier of [...undeclared].sort()) {
    errors.push(`${relative}: imports ${specifier}, which ${manifest.name} does not declare as a dependency`);
  }
}

if (errors.length > 0) {
  throw new Error(`source policy lint failed:\n${errors.join("\n")}`);
}
console.log(`source policy lint passed (${files.length} files)`);
