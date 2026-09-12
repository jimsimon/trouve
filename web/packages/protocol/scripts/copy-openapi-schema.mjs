import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = fileURLToPath(new URL("../", import.meta.url));

const sourcePath = resolve(
  packageRoot,
  "../../../crates/trouve-server/tests/snapshots/openapi.json",
);
const destinationPath = resolve(packageRoot, "src/generated/protocol-openapi.json");
mkdirSync(dirname(destinationPath), { recursive: true });
copyFileSync(sourcePath, destinationPath);
