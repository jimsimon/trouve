const LANGUAGE_BY_EXTENSION: Readonly<Record<string, string>> = Object.freeze({
  bash: "shell", c: "c", cc: "cpp", conf: "ini", cpp: "cpp", cs: "csharp",
  css: "css", go: "go", h: "c", hpp: "cpp", htm: "html", html: "html",
  ini: "ini", java: "java", js: "javascript", json: "json", jsx: "jsx",
  kt: "kotlin", kts: "kotlin", less: "css", markdown: "markdown", md: "markdown",
  php: "php", py: "python", rb: "ruby", rs: "rust", sass: "css", scss: "css",
  sh: "shell", sql: "sql", svelte: "html", svg: "xml", swift: "swift",
  toml: "toml", ts: "typescript", tsx: "tsx", vue: "html", xml: "xml",
  yaml: "yaml", yml: "yaml", zsh: "shell",
});

/** Keep file-language inference broad enough to match syntect's native file
 * preview instead of silently rendering every non-JS workspace as plain text. */
export const languageForPath = (path: string): string => {
  const base = path.split("/").at(-1)?.toLowerCase() ?? "";
  if (base === "dockerfile") return "dockerfile";
  if (base === "makefile" || base === "gnumakefile") return "makefile";
  if (base === "cargo.lock") return "toml";
  const extension = base.split(".").at(-1) ?? "";
  return LANGUAGE_BY_EXTENSION[extension] ?? "text";
};
