# Servo embedding qualification

This disposable harness embeds the Servo 2026-08-02 nightly directly in a
winit window, pinned to upstream revision
`35672cc3d4beb768489f5218e73bee7aff0ddb01`. It paints one webview over the
full client area, so it has no servoshell address bar, tabs, or other browser
chrome. Normal operating-system window decorations remain.

The crate is intentionally excluded from the root Cargo workspace and carries
its own lockfile. The pinned Servo nightly and trouve-server use incompatible
`libsqlite3-sys` link versions; Cargo cannot resolve them into the root
workspace lock. The harness therefore cannot link or start
trouve-server. It requires an explicit TROUVE_SERVER_URL, verifies the protocol
version, and reaches it only through trouve-desktop-host's hardened loopback
gateway.

Both Servo storage and host preferences use retained temporary directories.
The process cannot open Trouve's default database.

## Run

Build the desktop frontend:

~~~bash
cd web/app-ui
node scripts/generate-runtime-validators.mjs --check
node_modules/.bin/tsc -p tsconfig.json --noEmit
node_modules/.bin/tsc -p tsconfig.worker.json --noEmit
node_modules/.bin/vite build --mode desktop
~~~

Start a current-protocol trouve-server with an isolated TROUVE_DATA_DIR and
TROUVE_CONFIG, then run:

~~~bash
TROUVE_APP_UI_DIST="$PWD/web/app-ui/dist/desktop" \
TROUVE_SERVER_URL="http://127.0.0.1:<port>" \
cargo run \
  --manifest-path crates/trouve-servo-embed-preview/Cargo.toml \
  --locked
~~~

`TROUVE_APP_UI_DIST` is read at process startup by both Servo and Wry; changing
the Vite output never requires recompiling either Rust host. For live frontend
development, start the desktop-mode Vite server instead:

~~~bash
cd web/app-ui
TROUVE_APP_UI_DEV_URL=http://127.0.0.1:5173 \
  npm run dev
~~~

Then replace `TROUVE_APP_UI_DIST` in the preview command with
`TROUVE_APP_UI_DEV_URL=http://127.0.0.1:5173`. The preview remains on the
desktop gateway origin. Only Vite's HTTP assets and exact loopback HMR socket
are proxied/allowlisted; native and `/v1` routes do not move to the development
server.

Servo's mozangle build requires Clang, libclang, LLVM, CMake, and the normal
native graphics development packages. On this qualification host the signed
Arch clang, llvm, and llvm-libs packages can be extracted from the package
cache without installing them; point PATH, LD_LIBRARY_PATH, LIBCLANG_PATH,
LLVM_CONFIG_PATH, and CLANG_PATH at the extracted tree before invoking Cargo.

## Scope

The adapter supplies direct rendering, recreation through process restart,
resize/DPI, focus, keyboard, IME, pointer, wheel, touch, theme, animation
pumping, exact-origin navigation, temporary storage, and clean shutdown. It
loads the real Lit application from the same packaged, runtime-directory, or
loopback Vite source policy as Wry, including @lit/context, @lit-labs/signals,
WebAwesome, and the hard-widget implementations, so those surfaces can be
qualified in the embedded engine rather than in servoshell.

An earlier native-Wayland smoke run with the Servo 0.4.0 release, before the
current nightly pin, successfully created the window, served the desktop
assets, and completed protocol requests through the gateway. That release
also rejected CSS used by the current
frontend, including `:has()`, `color-scheme`, several accessibility media
queries, `text-overflow`, `user-select`, `resize`, and `touch-action`. Those are
open compatibility and visual-parity failures, not acceptable differences or a
qualification pass.

The pinned nightly has keyboard-driven selection in editable controls. Servo's
[upstream selection issue](https://github.com/servo/servo/issues/38124) for
mouse/touch selection of ordinary document text remains open, so drag selection
is still a qualification failure rather than an embedder input bug we can claim
to solve locally.

This does not promote Servo. Accessibility actions, native capability
adapters, clipboard behavior, dialog controls, drag/drop, downloads, DevTools,
crash/OOM recovery, memory/performance budgets, visual parity, and all release
targets remain qualification gates in ADR 0023 and the migration plan.
