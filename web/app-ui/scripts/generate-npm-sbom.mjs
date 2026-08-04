import { mkdir, readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const lockPath = fileURLToPath(new URL("../package-lock.json", import.meta.url));
const outputPath = fileURLToPath(
  new URL("../dist/npm-sbom.cdx.json", import.meta.url),
);

const lock = JSON.parse(await readFile(lockPath, "utf8"));
if (lock.lockfileVersion !== 3 || typeof lock.packages !== "object") {
  throw new Error("expected a package-lock v3 packages inventory");
}

const packageName = (packagePath, metadata) => {
  if (typeof metadata.name === "string") return metadata.name;
  const marker = "node_modules/";
  const index = packagePath.lastIndexOf(marker);
  return index < 0 ? packagePath : packagePath.slice(index + marker.length);
};

const purlName = (name) =>
  name.startsWith("@") ? `%40${name.slice(1)}` : name;

const integrityHash = (integrity) => {
  if (typeof integrity !== "string") return [];
  const candidate = integrity.split(/\s+/u)[0];
  const separator = candidate.indexOf("-");
  if (separator < 1) return [];
  const algorithm = {
    sha256: "SHA-256",
    sha384: "SHA-384",
    sha512: "SHA-512",
  }[candidate.slice(0, separator).toLowerCase()];
  if (algorithm === undefined) return [];
  const digest = candidate.slice(separator + 1);
  try {
    return [{
      alg: algorithm,
      content: Buffer.from(digest, "base64").toString("hex"),
    }];
  } catch {
    return [];
  }
};

const components = Object.entries(lock.packages)
  .filter(([packagePath]) => packagePath !== "")
  .map(([packagePath, metadata]) => {
    const name = packageName(packagePath, metadata);
    const version = metadata.version;
    if (typeof name !== "string" || typeof version !== "string") {
      throw new Error(`incomplete package metadata at ${packagePath}`);
    }
    const purl = `pkg:npm/${purlName(name)}@${version}`;
    const component = {
      type: "library",
      "bom-ref": `${purl}?path=${encodeURIComponent(packagePath)}`,
      name,
      version,
      purl,
      properties: [
        {
          name: "npm:scope",
          value: metadata.dev === true || metadata.devOptional === true
            ? "development"
            : "runtime/transitive",
        },
        { name: "npm:optional", value: String(metadata.optional === true) },
      ],
    };
    if (typeof metadata.license === "string") {
      component.licenses = [{ expression: metadata.license }];
    }
    const hashes = integrityHash(metadata.integrity);
    if (hashes.length > 0) component.hashes = hashes;
    if (typeof metadata.resolved === "string") {
      component.externalReferences = [
        { type: "distribution", url: metadata.resolved },
      ];
    }
    return component;
  })
  .sort((left, right) =>
    left.name.localeCompare(right.name)
      || left.version.localeCompare(right.version)
      || left["bom-ref"].localeCompare(right["bom-ref"]),
  );

const rootMetadata = lock.packages[""] ?? {};
const rootName = rootMetadata.name ?? lock.name;
const rootVersion = rootMetadata.version ?? lock.version;
if (typeof rootName !== "string" || typeof rootVersion !== "string") {
  throw new Error("missing root package name or version");
}

const document = {
  bomFormat: "CycloneDX",
  specVersion: "1.6",
  version: 1,
  metadata: {
    component: {
      type: "application",
      "bom-ref": `pkg:npm/${purlName(rootName)}@${rootVersion}`,
      name: rootName,
      version: rootVersion,
      purl: `pkg:npm/${purlName(rootName)}@${rootVersion}`,
    },
  },
  components,
};

await mkdir(fileURLToPath(new URL("../dist/", import.meta.url)), {
  recursive: true,
});
await writeFile(outputPath, `${JSON.stringify(document, null, 2)}\n`);
console.log(`npm SBOM: CycloneDX 1.6, ${components.length} locked components`);
