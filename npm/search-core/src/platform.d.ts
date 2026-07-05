/** npm package name for the current OS/arch/libc. */
export declare function platformPackageName(): string

/** Absolute path to the bundled binary, or the bare binary name for PATH fallback. */
export declare function resolveBinaryPath(): string
