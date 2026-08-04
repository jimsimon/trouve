import { copyFileSync } from "node:fs";
import { resolve } from "node:path";

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
  copyFileSync(resolve(source), resolve(destination));
}
