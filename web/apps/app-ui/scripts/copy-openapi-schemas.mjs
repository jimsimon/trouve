import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const appRoot = fileURLToPath(new URL("../", import.meta.url));

// The server protocol snapshot is owned by @trouve-ai/protocol.
const sourcePath = resolve(
  appRoot,
  "../../../crates/trouve-desktop-host/tests/snapshots/openapi.json",
);
const destinationPath = resolve(appRoot, "src/generated/host-openapi.json");
mkdirSync(dirname(destinationPath), { recursive: true });
copyFileSync(sourcePath, destinationPath);
