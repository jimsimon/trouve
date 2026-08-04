import type {
  ProtocolRegisterWorkspaceRequest,
  ProtocolWorkspace,
} from "../services/protocol-client.js";

export interface DirectoryPickerAction {
  readonly pickDirectory: () => Promise<string | undefined>;
}

export interface WorkspaceRegistrationAction {
  readonly registerWorkspace: (
    request: ProtocolRegisterWorkspaceRequest,
  ) => Promise<ProtocolWorkspace>;
}

export interface WorkspaceRegistrationDraft {
  readonly path: string;
  readonly name: string;
}

export interface WorkspaceRegistrationErrors {
  readonly path?: string;
}

export const emptyWorkspaceRegistrationDraft = (): WorkspaceRegistrationDraft => ({
  path: "",
  name: "",
});

export const validateWorkspaceRegistration = (
  draft: WorkspaceRegistrationDraft,
): WorkspaceRegistrationErrors =>
  draft.path.trim() === ""
    ? { path: "Enter an absolute repository path on the server host." }
    : {};

export const workspaceRegistrationRequest = (
  draft: WorkspaceRegistrationDraft,
): ProtocolRegisterWorkspaceRequest => {
  const errors = validateWorkspaceRegistration(draft);
  if (errors.path !== undefined) throw new TypeError(errors.path);
  const name = draft.name.trim();
  return {
    path: draft.path.trim(),
    ...(name === "" ? {} : { name }),
  };
};

/** Native selection is only path discovery. Registration remains an ordinary
 * trouve-server protocol mutation, preserving the client boundary in ADR 0023. */
export const pickAndRegisterWorkspace = async (
  picker: DirectoryPickerAction,
  protocol: WorkspaceRegistrationAction,
): Promise<ProtocolWorkspace | undefined> => {
  const path = await picker.pickDirectory();
  if (path === undefined) return undefined;
  if (path === "" || /[\u0000-\u001f\u007f]/u.test(path)) {
    throw new TypeError("desktop directory picker returned an invalid path");
  }
  return protocol.registerWorkspace({ path });
};
