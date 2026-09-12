import { createRequire } from "node:module";

import { defineConfig } from "vite";

export default defineConfig({
  resolve: {
    alias: {
      // Same as the desktop build: this package's DOM build calls
      // document.createElement at module evaluation, which would terminate
      // the content worker; Node's default condition resolves the DOM-free entry.
      "decode-named-character-reference": createRequire(import.meta.url).resolve(
        "decode-named-character-reference",
      ),
    },
  },
  worker: {
    format: "es",
  },
});
