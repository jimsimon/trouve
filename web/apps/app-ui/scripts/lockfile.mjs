/** Derive a package name from package-lock v3 metadata. */
export const packageName = (packagePath, metadata) => {
  if (typeof metadata.name === "string") return metadata.name;
  const marker = "node_modules/";
  const index = packagePath.lastIndexOf(marker);
  return index < 0 ? packagePath : packagePath.slice(index + marker.length);
};

/**
 * Whether a package-lock v3 entry is an installed third-party dependency.
 * The workspace lockfile also records the root, every first-party workspace
 * member (`apps/*`, `packages/*`), and the `node_modules/@trouve-ai/*`
 * symlinks pointing at them; none of those are third-party inventory.
 */
export const isThirdPartyEntry = (packagePath, metadata) =>
  packagePath !== ""
  && metadata.link !== true
  && packagePath.includes("node_modules/");
