// The review-ui image is built from the `web/` workspace root with hand-listed
// COPY instructions (see apps/review-ui/Dockerfile). Two things must stay in
// step with the workspace as packages are added or wired together:
//
// 1. `npm ci` validates the lockfile against every workspace manifest, so each
//    member's package.json has to be staged before the install layer.
// 2. Shared packages are consumed as TypeScript source through their package
//    exports, so the sources (and tsconfig) of every `@trouve-ai/*` package in
//    review-ui's dependency closure have to be copied before the build runs.
//
// A miss in either list only surfaces when the image is built from a clean
// context, so this check reproduces the Dockerfile's staging rules against the
// current workspace.
import { readFile } from "node:fs/promises";
import { join } from "node:path";

import { workspaceMembers, workspaceRoot } from "./workspace-sources.mjs";

const app = "apps/review-ui";
const dockerfilePath = join(workspaceRoot, app, "Dockerfile");
const dockerfile = await readFile(dockerfilePath, "utf8");
const lines = dockerfile
  .split("\n")
  .map((line) => line.trim())
  .filter((line) => line !== "" && !line.startsWith("#"));

const stageEnd = lines.findIndex((line, index) => index > 0 && line.startsWith("FROM "));
const buildStage = stageEnd === -1 ? lines : lines.slice(0, stageEnd);
const installIndex = buildStage.findIndex((line) => /^RUN\s+npm ci\b/u.test(line));
const buildIndex = buildStage.findIndex((line) => /^RUN\s+npm run build\b/u.test(line));

const errors = [];
if (installIndex === -1) errors.push("build stage has no `RUN npm ci` instruction");
if (buildIndex === -1) errors.push("build stage has no `RUN npm run build` instruction");

/** Paths named by COPY instructions in `[from, to)`, as written in the Dockerfile. */
const copiedPaths = (from, to) => {
  const paths = new Set();
  for (const line of buildStage.slice(from, to)) {
    const match = /^COPY\s+(?:--\S+\s+)*(.+)$/u.exec(line);
    if (match === null) continue;
    const operands = match[1].split(/\s+/u);
    operands.pop(); // destination
    for (const source of operands) paths.add(source);
  }
  return paths;
};

const members = await workspaceMembers();
const manifests = new Map();
for (const member of members) {
  const manifest = JSON.parse(await readFile(join(workspaceRoot, member, "package.json"), "utf8"));
  manifests.set(manifest.name, { member, manifest });
}

if (errors.length === 0) {
  const preInstall = copiedPaths(0, installIndex);
  for (const required of ["package.json", "package-lock.json"]) {
    if (!preInstall.has(required)) {
      errors.push(`${required} is not copied before \`npm ci\``);
    }
  }
  for (const member of members) {
    if (!preInstall.has(`${member}/package.json`)) {
      errors.push(
        `${member}/package.json is not copied before \`npm ci\`; npm cannot validate the `
        + "lockfile (or link the workspace) without every member manifest",
      );
    }
  }

  // review-ui's transitive @trouve-ai/* closure, all consumed as source.
  const closure = new Set();
  const visit = (name) => {
    const entry = manifests.get(name);
    if (entry === undefined || closure.has(name)) return;
    closure.add(name);
    for (const dependency of Object.keys(entry.manifest.dependencies ?? {})) visit(dependency);
  };
  const appEntry = [...manifests.values()].find((entry) => entry.member === app);
  if (appEntry === undefined) {
    errors.push(`${app} is not a workspace member`);
  } else {
    visit(appEntry.manifest.name);
    const preBuild = copiedPaths(installIndex + 1, buildIndex);
    for (const name of closure) {
      const { member } = manifests.get(name);
      const wanted = member === app
        ? [`${member}/src`]
        : [`${member}/src`, `${member}/tsconfig.json`];
      for (const path of wanted) {
        if (!preBuild.has(path)) {
          errors.push(
            `${path} is not copied between \`npm ci\` and \`npm run build\`, but `
            + `${appEntry.manifest.name} depends on ${name}`,
          );
        }
      }
    }
  }
}

if (errors.length > 0) {
  console.error(`review-ui Dockerfile check failed (${app}/Dockerfile):`);
  for (const error of errors) console.error(`  - ${error}`);
  process.exit(1);
}
console.log(
  `review-ui Dockerfile check passed (${members.length} manifests staged, `
  + "dependency closure copied)",
);
