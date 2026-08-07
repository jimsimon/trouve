import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const appRoot = fileURLToPath(new URL("../", import.meta.url));

const copies = [
  [
    "../../crates/trouve-server/tests/snapshots/openapi.json",
    "src/generated/protocol-openapi.json",
  ],
  [
    "../../crates/trouve-desktop-host/tests/snapshots/openapi.json",
    "src/generated/host-openapi.json",
  ],
];

for (const [source, destination] of copies) {
  const sourcePath = resolve(appRoot, source);
  const destinationPath = resolve(appRoot, destination);
  mkdirSync(dirname(destinationPath), { recursive: true });
  copyFileSync(sourcePath, destinationPath);
}
