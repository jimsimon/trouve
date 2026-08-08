/** Derive a package name from package-lock v3 metadata. */
export const packageName = (packagePath, metadata) => {
  if (typeof metadata.name === "string") return metadata.name;
  const marker = "node_modules/";
  const index = packagePath.lastIndexOf(marker);
  return index < 0 ? packagePath : packagePath.slice(index + marker.length);
};
