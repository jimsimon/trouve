// Desktop host API validators. The server protocol validators are owned by
// @trouve-ai/protocol; this script shares its CSP-safe AJV pipeline.
import {
  componentRef,
  generateModule,
  parseGeneratorArguments,
  readJson,
  writeGeneratedModules,
} from "@trouve-ai/protocol/codegen/runtime-validators";

const appRoot = new URL("../", import.meta.url);
const { checkOnly } = parseGeneratorArguments();

const hostSchemaId = "urn:trouve:desktop-host-openapi";
const hostDocument = readJson(new URL("src/generated/host-openapi.json", appRoot));
const hostSource = generateModule({
  document: hostDocument,
  schemaId: hostSchemaId,
  validatorNamespace: "host",
  validators: {
    hostBootstrap: componentRef(hostSchemaId, "HostBootstrap"),
    hostPreferences: componentRef(hostSchemaId, "HostPreferences"),
    hostLifecycleBatch: componentRef(hostSchemaId, "HostLifecycleBatch"),
    pickDirectoryResponse: componentRef(hostSchemaId, "PickDirectoryResponse"),
    pickFilesResponse: componentRef(hostSchemaId, "PickFilesResponse"),
    readClipboardImageResponse: componentRef(hostSchemaId, "ReadClipboardImageResponse"),
  },
});

writeGeneratedModules({
  outputs: [["src/generated/host-validators.ts", hostSource]],
  packageRoot: appRoot,
  checkOnly,
  regenerateCommand: "npm run generate:validators",
});
