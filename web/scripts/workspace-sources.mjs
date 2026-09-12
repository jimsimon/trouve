// Enumerates the source files that the workspace-wide hygiene checks cover.
import { access, readdir, readFile } from "node:fs/promises";
import { extname, join } from "node:path";
import { fileURLToPath } from "node:url";

export const workspaceRoot = fileURLToPath(new URL("../", import.meta.url));

// Members not yet on the shared Lit toolchain; every workspace member is
// covered now that the review UI is a shell over @trouve-ai/code-review.
const pendingLitPort = new Set();

const exists = async (path) => {
  try {
    await access(path);
    return true;
  } catch {
    return false;
  }
};

/** Workspace members (relative to the web root) covered by the shared checks. */
export const workspaceMembers = async () => {
  const manifest = JSON.parse(await readFile(join(workspaceRoot, "package.json"), "utf8"));
  const members = [];
  for (const pattern of manifest.workspaces) {
    if (!pattern.endsWith("/*")) throw new Error(`unsupported workspace glob: ${pattern}`);
    const parent = pattern.slice(0, -2);
    const entries = await readdir(join(workspaceRoot, parent), { withFileTypes: true });
    for (const entry of entries) {
      const member = `${parent}/${entry.name}`;
      if (!entry.isDirectory() || pendingLitPort.has(member)) continue;
      if (await exists(join(workspaceRoot, member, "package.json"))) members.push(member);
    }
  }
  return members.sort();
};

const visit = async (root, relative, extensions, files) => {
  const entries = await readdir(join(root, relative), { withFileTypes: true });
  for (const entry of entries) {
    const child = join(relative, entry.name);
    if (entry.isDirectory()) await visit(root, child, extensions, files);
    else if (extensions.has(extname(entry.name))) files.push(child);
  }
};

/** Files (relative to the web root) under `directories` of each member plus
 * any of `topLevel` that exist, filtered by extension. */
export const workspaceSources = async ({ directories, topLevel, extensions }) => {
  const files = [];
  for (const member of await workspaceMembers()) {
    const memberRoot = join(workspaceRoot, member);
    for (const directory of directories) {
      if (await exists(join(memberRoot, directory))) {
        const memberFiles = [];
        await visit(memberRoot, directory, extensions, memberFiles);
        files.push(...memberFiles.map((file) => join(member, file)));
      }
    }
    for (const file of topLevel) {
      if (await exists(join(memberRoot, file))) files.push(join(member, file));
    }
  }
  return files.sort();
};
