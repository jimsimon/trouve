import type {
  ProtocolAgentPersona,
  ProtocolUpdateThreadRequest,
} from "../services/protocol-client.js";

/** Build one atomic thread update for a mode selection. A mode's default model
 * takes effect with the mode, and options are cleared only when that changes
 * the effective model. */
export const threadModeSettingRequest = (
  modes: readonly Pick<ProtocolAgentPersona, "id" | "default_model">[],
  modeId: string,
  currentModel: string,
): ProtocolUpdateThreadRequest => {
  const mode = modes.find((candidate) => candidate.id === modeId);
  const defaultModel = mode?.default_model?.trim() ?? "";
  const nextModel = defaultModel || currentModel;
  return {
    mode: modeId,
    ...(defaultModel === "" ? {} : { model: defaultModel }),
    ...(nextModel === currentModel ? {} : { model_options: {} }),
  };
};
