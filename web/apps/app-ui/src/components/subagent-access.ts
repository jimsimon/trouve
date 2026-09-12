import type {
  ProtocolAgentPersona,
  ProtocolThread,
} from "@trouve-ai/protocol/client";

type SubagentMode = Pick<ProtocolAgentPersona, "id" | "read_only">;
type SubagentThread = Pick<ProtocolThread, "mode" | "spawned">;

/**
 * Exploration and audit children inherit their mode's read-only contract.
 * Unknown modes fail closed until the workspace-specific catalog is loaded.
 */
export const subagentThreadIsReadOnly = (
  thread: SubagentThread,
  modes: readonly SubagentMode[],
): boolean => {
  if (thread.spawned !== true) return false;
  const mode = modes.find((candidate) => candidate.id === thread.mode);
  return mode === undefined || mode.read_only === true;
};
