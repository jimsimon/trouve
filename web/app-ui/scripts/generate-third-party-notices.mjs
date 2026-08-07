import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const lockPath = fileURLToPath(new URL("../package-lock.json", import.meta.url));
const noticePath = fileURLToPath(new URL("../THIRD_PARTY_NOTICES.md", import.meta.url));
const check = process.argv.includes("--check");

const approvedLicenses = new Set([
  "(CC-BY-4.0 AND OFL-1.1 AND MIT)",
  "(MIT OR CC0-1.0)",
  "0BSD",
  "Apache-2.0",
  "BSD-2-Clause",
  "BSD-3-Clause",
  "ISC",
  "MIT",
  "MPL-2.0",
  "Python-2.0",
]);

const packageName = (packagePath, metadata) => {
  if (typeof metadata.name === "string") return metadata.name;
  const marker = "node_modules/";
  const index = packagePath.lastIndexOf(marker);
  return index < 0 ? packagePath : packagePath.slice(index + marker.length);
};

const lock = JSON.parse(await readFile(lockPath, "utf8"));
const packages = Object.entries(lock.packages ?? {})
  .filter(([packagePath]) => packagePath !== "")
  .map(([packagePath, metadata]) => ({
    name: packageName(packagePath, metadata),
    version: metadata.version,
    license: metadata.license,
    development: metadata.dev === true || metadata.devOptional === true,
  }));

const invalid = packages.filter(
  ({ name, version, license }) =>
    typeof name !== "string"
    || typeof version !== "string"
    || typeof license !== "string"
    || !approvedLicenses.has(license),
);
if (invalid.length > 0) {
  throw new Error(
    `unreviewed package metadata:\n${invalid
      .map(({ name, version, license }) => `- ${name}@${version ?? "?"}: ${license ?? "missing license"}`)
      .join("\n")}`,
  );
}

const prohibited = packages.filter(({ name }) =>
  name.toLowerCase().includes("webawesome-pro"),
);
if (prohibited.length > 0) {
  throw new Error("WebAwesome Pro must not enter the dependency graph without an ADR and license review");
}

packages.sort((left, right) =>
  left.name.localeCompare(right.name) || left.version.localeCompare(right.version),
);

const lines = [
  "# Third-party notices — Lit frontend",
  "",
  "This file is generated from `web/app-ui/package-lock.json` by",
  "`npm run generate:notices`. It inventories both runtime and development",
  "dependencies; the packaged Vite assets contain only the runtime subset.",
  "Review each newly introduced license before regenerating this file.",
  "",
  "WebAwesome is the MIT-licensed Free distribution (`@awesome.me/webawesome`).",
  "The Pro distribution is prohibited by the generator unless a future ADR and",
  "license review deliberately change that policy.",
  "",
  "| Package | Version | License | Scope |",
  "| --- | --- | --- | --- |",
  ...packages.map(({ name, version, license, development }) =>
    `| ${name.replaceAll("|", "\\|")} | ${version} | ${license} | ${development ? "development" : "runtime/transitive"} |`,
  ),
  "",
];
const generated = lines.join("\n");

if (check) {
  let existing = "";
  try {
    existing = await readFile(noticePath, "utf8");
  } catch {
    // Report the same actionable drift error for a missing file.
  }
  if (existing !== generated) {
    throw new Error(`third-party notices are stale; run npm run generate:notices from ${root}`);
  }
} else {
  await writeFile(noticePath, generated);
}
