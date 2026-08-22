import createClient, { type Client } from "openapi-fetch";

import type {
  components as ProtocolComponents,
  paths as ProtocolPaths,
} from "../generated/protocol.js";
import {
  CursorEventStream,
  type EventSourceFactory,
  type SafeStreamDiagnostic,
} from "./cursor-event-stream.js";

type ValidateFunction = (value: unknown) => boolean;

export type ProtocolEventEnvelope =
  ProtocolComponents["schemas"]["EventEnvelope"];
export type ProtocolScope = ProtocolComponents["schemas"]["Scope"];
export type ProtocolSession = ProtocolComponents["schemas"]["Session"];
export type ProtocolForkCheckpointResponse =
  ProtocolComponents["schemas"]["ForkCheckpointResponse"];
export type ProtocolCreateSessionRequest =
  ProtocolComponents["schemas"]["CreateSessionRequest"];
export type ProtocolGeneratedSessionTitle =
  ProtocolComponents["schemas"]["GeneratedSessionTitle"];
export type ProtocolUpdateSessionRequest =
  ProtocolComponents["schemas"]["UpdateSessionRequest"];
export type ProtocolSessionSummary =
  ProtocolComponents["schemas"]["SessionSummary"];
export type ProtocolSessionSummariesSnapshot =
  ProtocolComponents["schemas"]["SessionSummariesSnapshot"];
export type ProtocolWorkspace = ProtocolComponents["schemas"]["Workspace"];
export type ProtocolRegisterWorkspaceRequest =
  ProtocolComponents["schemas"]["RegisterWorkspaceRequest"];
export type ProtocolBranchList = ProtocolComponents["schemas"]["BranchList"];
export type ProtocolGithubPrList =
  ProtocolComponents["schemas"]["GithubPrList"];
export type ProtocolPrInfo = ProtocolComponents["schemas"]["PrInfo"];
export type ProtocolPrDetail = ProtocolComponents["schemas"]["PrDetail"];
export type ProtocolPrDetailSection =
  ProtocolComponents["schemas"]["PrDetailSection"];
export type ProtocolPrFileDiff = ProtocolComponents["schemas"]["PrFileDiff"];
export type ProtocolPrActionRequest =
  ProtocolComponents["schemas"]["PrActionRequest"];
export type ProtocolCreatePrRequest =
  ProtocolComponents["schemas"]["CreatePrRequest"];
export type ProtocolAgentPersona = ProtocolComponents["schemas"]["AgentPersona"];
export type ProtocolPersonaInfo = ProtocolComponents["schemas"]["PersonaInfo"];
export type ProtocolUpsertPersonaRequest =
  ProtocolComponents["schemas"]["UpsertPersonaRequest"];
export type ProtocolSetGlobalDefaultsRequest =
  ProtocolComponents["schemas"]["SetGlobalDefaultsRequest"];
export type ProtocolSetDefaultModelRequest =
  ProtocolComponents["schemas"]["SetDefaultModelRequest"];
export type ProtocolSetDefaultPermissionModeRequest =
  ProtocolComponents["schemas"]["SetDefaultPermissionModeRequest"];
export type ProtocolModelInfo = ProtocolComponents["schemas"]["ModelInfo"];
export type ProtocolThread = ProtocolComponents["schemas"]["Thread"];
export type ProtocolThreadStatus = ProtocolComponents["schemas"]["ThreadStatus"];
export type ProtocolThreadViewSnapshot =
  ProtocolComponents["schemas"]["ThreadViewSnapshot"];
export type ProtocolThreadToolDetails =
  ProtocolComponents["schemas"]["ThreadToolDetails"];
export type ProtocolTodoItem = ProtocolComponents["schemas"]["TodoItem"];
export type ProtocolCreateThreadRequest =
  ProtocolComponents["schemas"]["CreateThreadRequest"];
export type ProtocolUpdateThreadRequest =
  ProtocolComponents["schemas"]["UpdateThreadRequest"];
export type ProtocolQueuedPrompt = ProtocolComponents["schemas"]["QueuedPrompt"];
export type ProtocolAttachment = ProtocolComponents["schemas"]["Attachment"];
export type ProtocolAttachmentUpload =
  ProtocolComponents["schemas"]["AttachmentUpload"];
export type ProtocolUpdateQueuedPromptRequest =
  ProtocolComponents["schemas"]["UpdateQueuedPromptRequest"];
export type ProtocolSendMessageRequest =
  ProtocolComponents["schemas"]["SendMessageRequest"];
export type ProtocolExecuteCommandRequest =
  ProtocolComponents["schemas"]["ExecuteCommandRequest"];
export type ProtocolCommandResult =
  ProtocolComponents["schemas"]["CommandResult"];
export type ProtocolSteerTurnRequest =
  ProtocolComponents["schemas"]["SteerTurnRequest"];
export type ProtocolTurnAccepted = ProtocolComponents["schemas"]["TurnAccepted"];
export type ProtocolUsageSummary = ProtocolComponents["schemas"]["UsageSummary"];
export type ProtocolResolveApprovalRequest =
  ProtocolComponents["schemas"]["ResolveApprovalRequest"];
export type ProtocolResolveQuestionRequest =
  ProtocolComponents["schemas"]["ResolveQuestionRequest"];
export type ProtocolSessionDiffFileSummary =
  ProtocolComponents["schemas"]["SessionDiffFileSummary"];
export type ProtocolSessionDiffSummary =
  ProtocolComponents["schemas"]["SessionDiffSummary"];
export type ProtocolSessionFileDiff =
  ProtocolComponents["schemas"]["SessionFileDiff"];
export type ProtocolRestoreDirection =
  ProtocolComponents["schemas"]["RestoreDirection"];
export type ProtocolRelativeRestoreDirection = Exclude<
  ProtocolRestoreDirection,
  "exact"
>;
export type ProtocolDirEntry = ProtocolComponents["schemas"]["DirEntry"];
export type ProtocolFileContent = ProtocolComponents["schemas"]["FileContent"];
export type ProtocolTerminalInfo = ProtocolComponents["schemas"]["TerminalInfo"];
export type ProtocolTerminalReplayStart =
  ProtocolComponents["schemas"]["TerminalReplayStart"];
export type ProtocolServerInfo = ProtocolComponents["schemas"]["ServerInfo"];
export type ProtocolServerProjection =
  ProtocolComponents["schemas"]["ServerProjection"];
export type ProtocolProvidersResponse =
  ProtocolComponents["schemas"]["ProvidersResponse"];
export type ProtocolProviderInfo = ProtocolComponents["schemas"]["ProviderInfo"];
export type ProtocolKnownProvider = ProtocolComponents["schemas"]["KnownProvider"];
export type ProtocolUpsertProviderRequest =
  ProtocolComponents["schemas"]["UpsertProviderRequest"];
export type ProtocolLoginStarted = ProtocolComponents["schemas"]["LoginStarted"];
export type ProtocolLoginStatus = ProtocolComponents["schemas"]["LoginStatus"];
export type ProtocolSubscriptionHealth =
  ProtocolComponents["schemas"]["SubscriptionHealth"];
export type ProtocolLocalStatus = ProtocolComponents["schemas"]["LocalStatus"];
export type ProtocolLocalModelInfo =
  ProtocolComponents["schemas"]["LocalModelInfo"];
export type ProtocolLocalSearchResult =
  ProtocolComponents["schemas"]["LocalSearchResult"];
export type ProtocolAddLocalModelRequest =
  ProtocolComponents["schemas"]["AddLocalModelRequest"];
export type ProtocolAutomation = ProtocolComponents["schemas"]["Automation"];
export type ProtocolAutomationSchedule =
  ProtocolComponents["schemas"]["AutomationSchedule"];
export type ProtocolAutomationTemplate =
  ProtocolComponents["schemas"]["AutomationTemplate"];
export type ProtocolUpsertAutomationRequest =
  ProtocolComponents["schemas"]["UpsertAutomationRequest"];
export type ProtocolCodeReviewDashboard =
  ProtocolComponents["schemas"]["CodeReviewDashboard"];
export type ProtocolCodeReviewJob =
  ProtocolComponents["schemas"]["CodeReviewJob"];
export type ProtocolCodeReviewSettings =
  ProtocolComponents["schemas"]["CodeReviewSettings"];
export type ProtocolSetCodeReviewSettingsRequest =
  ProtocolComponents["schemas"]["SetCodeReviewSettingsRequest"];
export type ProtocolGithubAppStatus =
  ProtocolComponents["schemas"]["GithubAppStatus"];
export type ProtocolConfigureGithubAppRequest =
  ProtocolComponents["schemas"]["ConfigureGithubAppRequest"];
export type ProtocolReviewerProfile =
  ProtocolComponents["schemas"]["ReviewerProfile"];
export type ProtocolCodeReviewRepository =
  ProtocolComponents["schemas"]["CodeReviewRepository"];
export type ProtocolUpdateCodeReviewRepositoryRequest =
  ProtocolComponents["schemas"]["UpdateCodeReviewRepositoryRequest"];
export type ProtocolGitWorktreeSettings =
  ProtocolComponents["schemas"]["GitWorktreeSettings"];
export type ProtocolSetGitWorktreeSettingsRequest =
  ProtocolComponents["schemas"]["SetGitWorktreeSettingsRequest"];
export type ProtocolSkillsSettings =
  ProtocolComponents["schemas"]["SkillsSettings"];
export type ProtocolSetSkillsSettingsRequest =
  ProtocolComponents["schemas"]["SetSkillsSettingsRequest"];

export interface ProtocolCursorSnapshot<T> {
  readonly cursor: number;
  readonly value: T;
}

/** Keep tail snapshots large enough to avoid a visible first backfill. */
export const THREAD_VIEW_PAGE_SIZE = 256;
export type ProtocolGithubIntegration =
  ProtocolComponents["schemas"]["GithubIntegration"];
export type ProtocolAddGithubHostRequest =
  ProtocolComponents["schemas"]["AddGithubHostRequest"];
export type ProtocolMcpServerInfo =
  ProtocolComponents["schemas"]["McpServerInfo"];
export type ProtocolUpsertMcpServerRequest =
  ProtocolComponents["schemas"]["UpsertMcpServerRequest"];
export type ProtocolSetMcpServerEnabledRequest =
  ProtocolComponents["schemas"]["SetMcpServerEnabledRequest"];
export type ProtocolMcpLogs = ProtocolComponents["schemas"]["McpLogs"];
export type ProtocolCliInfo = ProtocolComponents["schemas"]["CliInfo"];
export type ProtocolCliList = ProtocolComponents["schemas"]["CliList"];
export type ProtocolCliInstallStatus =
  ProtocolComponents["schemas"]["CliInstallStatus"];

export type ProtocolIngressEvent =
  | {
      readonly kind: "known";
      readonly cursor: number;
      readonly envelope: ProtocolEventEnvelope;
    }
  | {
      readonly kind: "unknown";
      readonly cursor: number;
      readonly scope: ProtocolScope;
      readonly ts: string;
      readonly type: string;
    };

interface ProtocolValidators {
  readonly session: ValidateFunction;
  readonly sessions: ValidateFunction;
  readonly forkCheckpointResponse: ValidateFunction;
  readonly generatedSessionTitle: ValidateFunction;
  readonly summaries: ValidateFunction;
  readonly workspace: ValidateFunction;
  readonly workspaces: ValidateFunction;
  readonly branchList: ValidateFunction;
  readonly prInfo: ValidateFunction;
  readonly personas: ValidateFunction;
  readonly personaInfos: ValidateFunction;
  readonly models: ValidateFunction;
  readonly thread: ValidateFunction;
  readonly threads: ValidateFunction;
  readonly threadStatuses: ValidateFunction;
  readonly queuedPrompts: ValidateFunction;
  readonly turnAccepted: ValidateFunction;
  readonly commandResult: ValidateFunction;
  readonly skillsSettings: ValidateFunction;
  readonly usageSummary: ValidateFunction;
  readonly dirEntries: ValidateFunction;
  readonly paths: ValidateFunction;
  readonly fileContent: ValidateFunction;
  readonly terminalInfo: ValidateFunction;
  readonly terminalInfos: ValidateFunction;
  readonly serverInfo: ValidateFunction;
  readonly serverProjection: ValidateFunction;
  readonly providers: ValidateFunction;
  readonly provider: ValidateFunction;
  readonly knownProviders: ValidateFunction;
  readonly loginStarted: ValidateFunction;
  readonly loginStatus: ValidateFunction;
  readonly subscriptions: ValidateFunction;
  readonly localStatus: ValidateFunction;
  readonly localSearch: ValidateFunction;
  readonly automation: ValidateFunction;
  readonly automations: ValidateFunction;
  readonly automationTemplates: ValidateFunction;
  readonly codeReviewDashboard: ValidateFunction;
  readonly codeReviewJob: ValidateFunction;
  readonly codeReviewSettings: ValidateFunction;
  readonly githubAppStatus: ValidateFunction;
  readonly reviewerProfile: ValidateFunction;
  readonly codeReviewRepository: ValidateFunction;
  readonly gitWorktreeSettings: ValidateFunction;
  readonly githubIntegration: ValidateFunction;
  readonly mcpServers: ValidateFunction;
  readonly mcpLogs: ValidateFunction;
  readonly cliList: ValidateFunction;
  readonly cliInstallStatus: ValidateFunction;
  readonly knownEnvelope: ValidateFunction;
  readonly compatibleEnvelope: ValidateFunction;
  readonly knownEventTypes: ReadonlySet<string>;
}

const asRecord = (value: unknown): Record<string, unknown> | undefined =>
  typeof value === "object" && value !== null
    ? (value as Record<string, unknown>)
    : undefined;

const isNonnegativeInteger = (value: unknown): value is number =>
  typeof value === "number" && Number.isSafeInteger(value) && value >= 0;

const isSessionDiffSummary = (
  value: unknown,
): value is ProtocolSessionDiffSummary => {
  const record = asRecord(value);
  if (
    record === undefined ||
    !isNonnegativeInteger(record["additions"]) ||
    !isNonnegativeInteger(record["deletions"]) ||
    !Array.isArray(record["files"])
  ) return false;
  return record["files"].every((candidate) => {
    const file = asRecord(candidate);
    return file !== undefined &&
      typeof file["path"] === "string" &&
      file["path"] !== "" &&
      isNonnegativeInteger(file["additions"]) &&
      isNonnegativeInteger(file["deletions"]) &&
      typeof file["binary"] === "boolean";
  });
};

const isSessionFileDiff = (value: unknown): value is ProtocolSessionFileDiff => {
  const record = asRecord(value);
  return record !== undefined &&
    typeof record["path"] === "string" &&
    record["path"] !== "" &&
    typeof record["diff"] === "string";
};

const isThreadToolDetails = (value: unknown): value is ProtocolThreadToolDetails => {
  const record = asRecord(value);
  return record !== undefined
    && typeof record["call_id"] === "string"
    && record["call_id"] !== ""
    && Object.hasOwn(record, "args");
};

let loadedValidators: Promise<ProtocolValidators> | undefined;

/** Runtime schemas are sizeable generated code. Load them on the first
 * protocol response or event instead of charging every initial app render. */
const validators = (): Promise<ProtocolValidators> => {
  loadedValidators ??= import("../generated/protocol-validators.js").then(
    (precompiled) => ({
      ...precompiled,
      knownEventTypes: new Set(precompiled.knownEventTypes),
    }),
  );
  return loadedValidators;
};

let loadedPrDetailValidator: Promise<(value: unknown) => boolean> | undefined;
const prDetailValidator = (): Promise<(value: unknown) => boolean> => {
  loadedPrDetailValidator ??= import("./pr-detail-validator.js")
    .then(({ validatePrDetail }) => validatePrDetail);
  return loadedPrDetailValidator;
};

let loadedPrFileDiffValidator: Promise<(value: unknown) => boolean> | undefined;
const prFileDiffValidator = (): Promise<(value: unknown) => boolean> => {
  loadedPrFileDiffValidator ??= import("./pr-detail-validator.js")
    .then(({ validatePrFileDiff }) => validatePrFileDiff);
  return loadedPrFileDiffValidator;
};

const validateResponse = async <T>(
  name:
    | "Session"
    | "Session[]"
    | "ForkCheckpointResponse"
    | "GeneratedSessionTitle"
    | "SessionSummariesSnapshot"
    | "Workspace"
    | "Workspace[]"
    | "BranchList"
    | "PrInfo"
    | "PrInfo[]"
    | "AgentPersona[]"
    | "PersonaInfo[]"
    | "ModelInfo[]"
    | "Thread"
    | "Thread[]"
    | "ThreadStatus[]"
    | "ThreadViewSnapshot"
    | "QueuedPrompt[]"
    | "TurnAccepted"
    | "CommandResult"
    | "SkillsSettings"
    | "UsageSummary"
    | "DirEntry[]"
    | "Path[]"
    | "FileContent"
    | "TerminalInfo"
    | "TerminalInfo[]"
    | "ServerInfo"
    | "ServerProjection"
    | "ProvidersResponse"
    | "ProviderInfo"
    | "KnownProvider[]"
    | "LoginStarted"
    | "LoginStatus"
    | "SubscriptionHealth[]"
    | "LocalStatus"
    | "LocalSearchResult[]"
    | "Automation"
    | "Automation[]"
    | "AutomationTemplate[]"
    | "CodeReviewDashboard"
    | "CodeReviewJob"
    | "CodeReviewSettings"
    | "GithubAppStatus"
    | "ReviewerProfile"
    | "CodeReviewRepository"
    | "GitWorktreeSettings"
    | "GithubIntegration"
    | "McpServerInfo[]"
    | "McpLogs"
    | "CliList"
    | "CliInstallStatus",
  value: unknown,
  validate: (loaded: ProtocolValidators) => ValidateFunction,
): Promise<T> => {
  const loaded = await validators();
  if (!validate(loaded)(value)) {
    throw new ProtocolClientError("invalid-response", `server returned invalid ${name}`);
  }
  return value as T;
};

export const loadProtocolEventParser = async (): Promise<
  (value: unknown) => ProtocolIngressEvent
> => {
  const loaded = await validators();
  return (value: unknown): ProtocolIngressEvent => {
    if (loaded.knownEnvelope(value)) {
      const envelope = value as ProtocolEventEnvelope;
      return { kind: "known", cursor: envelope.cursor, envelope };
    }

    const record = asRecord(value);
    const type = record?.["type"];
    if (
      record === undefined ||
      typeof type !== "string" ||
      loaded.knownEventTypes.has(type)
    ) {
      throw new TypeError("invalid known protocol event");
    }
    if (!loaded.compatibleEnvelope(value)) {
      throw new TypeError("invalid protocol event envelope");
    }
    return {
      kind: "unknown",
      cursor: record["cursor"] as number,
      scope: record["scope"] as ProtocolScope,
      ts: record["ts"] as string,
      type,
    };
  };
};

export class ProtocolClientError extends Error {
  constructor(
    readonly kind: "request-failed" | "invalid-response" | "incompatible-protocol",
    message: string,
    readonly status?: number,
    readonly code?: string,
  ) {
    super(message);
    this.name = "ProtocolClientError";
  }
}

const MAX_PROTOCOL_ERROR_FIELD_LENGTH = 512;

// Generated protocol types and validators contain closed discriminated
// unions. A newer schema can therefore add a value this bundle cannot decode
// even when the server labels the change additive. Require the exact schema
// version this client was generated and tested against.
export const SUPPORTED_PROTOCOL_VERSION = "7.13";

export const assertProtocolCompatibility = (version: string): void => {
  if (version !== SUPPORTED_PROTOCOL_VERSION) {
    throw new ProtocolClientError(
      "incompatible-protocol",
      `server protocol ${version || "unknown"} is incompatible; expected exactly ${SUPPORTED_PROTOCOL_VERSION}`,
    );
  }
};

export class ProtocolClient {
  readonly #client: Client<ProtocolPaths>;
  readonly #baseUrl: string;
  readonly #fetch: typeof fetch;
  readonly #eventSourceFactory: EventSourceFactory | undefined;
  readonly #mutationHeaders: () => Readonly<Record<string, string>>;

  constructor(
    baseUrl: string,
    options: {
      readonly fetch?: typeof fetch;
      readonly eventSourceFactory?: EventSourceFactory;
      readonly mutationHeaders?: () => Readonly<Record<string, string>>;
    } = {},
  ) {
    this.#baseUrl = new URL(baseUrl).href;
    this.#fetch = options.fetch ?? globalThis.fetch.bind(globalThis);
    this.#eventSourceFactory = options.eventSourceFactory;
    this.#mutationHeaders = options.mutationHeaders ?? (() => ({}));
    this.#client = createClient<ProtocolPaths>({
      baseUrl: this.#baseUrl,
      fetch: this.#fetch,
    });
  }

  async #request(path: string, label: string, init?: RequestInit): Promise<Response> {
    let response: Response;
    try {
      response = await this.#fetch(new URL(path, this.#baseUrl), init);
    } catch {
      throw new ProtocolClientError("request-failed", `${label} request failed`);
    }
    if (!response.ok) {
      throw new ProtocolClientError(
        "request-failed",
        `${label} request failed`,
        response.status,
      );
    }
    return response;
  }

  async #validatedResponse<T>(
    response: Response,
    schemaName: Parameters<typeof validateResponse<T>>[0],
    validate: (loaded: ProtocolValidators) => ValidateFunction,
  ): Promise<T> {
    let value: unknown;
    try {
      value = await response.json();
    } catch {
      throw new ProtocolClientError("invalid-response", `server returned invalid ${schemaName}`);
    }
    return validateResponse<T>(schemaName, value, validate);
  }

  async #validatedJson<T>(
    path: string,
    label: string,
    schemaName: Parameters<typeof validateResponse<T>>[0],
    validate: (loaded: ProtocolValidators) => ValidateFunction,
    init: RequestInit = {},
  ): Promise<T> {
    const response = await this.#request(path, label, init);
    return this.#validatedResponse<T>(response, schemaName, validate);
  }

  async #validatedCursorJson<T>(
    path: string,
    label: string,
    schemaName: Parameters<typeof validateResponse<T>>[0],
    validate: (loaded: ProtocolValidators) => ValidateFunction,
    signal?: AbortSignal,
  ): Promise<ProtocolCursorSnapshot<T>> {
    const response = await this.#request(
      path,
      label,
      signal === undefined ? undefined : { signal },
    );
    const cursor = this.#responseCursor(response, label);
    return Object.freeze({
      cursor,
      value: await this.#validatedResponse<T>(response, schemaName, validate),
    });
  }

  async #parsePrDetail(response: Response): Promise<ProtocolPrDetail> {
    let value: unknown;
    try {
      value = await response.json();
    } catch {
      throw new ProtocolClientError("invalid-response", "server returned invalid PrDetail");
    }
    if (!(await prDetailValidator())(value)) {
      throw new ProtocolClientError("invalid-response", "server returned invalid PrDetail");
    }
    return value as ProtocolPrDetail;
  }

  async #parsePrFileDiff(response: Response): Promise<ProtocolPrFileDiff> {
    let value: unknown;
    try {
      value = await response.json();
    } catch {
      throw new ProtocolClientError("invalid-response", "server returned invalid PrFileDiff");
    }
    if (!(await prFileDiffValidator())(value)) {
      throw new ProtocolClientError("invalid-response", "server returned invalid PrFileDiff");
    }
    return value as ProtocolPrFileDiff;
  }

  #mutation(
    path: string,
    label: string,
    method: "POST" | "PUT" | "DELETE",
    body?: unknown,
    signal?: AbortSignal,
  ): Promise<Response> {
    return this.#request(path, label, {
      method,
      ...(signal === undefined ? {} : { signal }),
      headers: {
        ...this.#mutationHeaders(),
        ...(body === undefined ? {} : { "content-type": "application/json" }),
      },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
  }

  async #validatedMutation<T>(
    path: string,
    label: string,
    method: "POST" | "PUT" | "DELETE",
    schemaName: Parameters<typeof validateResponse<T>>[0],
    validate: (loaded: ProtocolValidators) => ValidateFunction,
    body?: unknown,
    signal?: AbortSignal,
  ): Promise<T> {
    const response = await this.#mutation(path, label, method, body, signal);
    return this.#validatedResponse<T>(response, schemaName, validate);
  }

  async #validatedCursorMutation<T>(
    path: string,
    label: string,
    method: "POST" | "PUT" | "DELETE",
    schemaName: Parameters<typeof validateResponse<T>>[0],
    validate: (loaded: ProtocolValidators) => ValidateFunction,
    body?: unknown,
  ): Promise<ProtocolCursorSnapshot<T>> {
    const response = await this.#mutation(path, label, method, body);
    const cursor = this.#responseCursor(response, label);
    return Object.freeze({
      cursor,
      value: await this.#validatedResponse<T>(response, schemaName, validate),
    });
  }

  #responseCursor(response: Response, label: string): number {
    const raw = response.headers.get("x-trouve-event-cursor");
    const cursor = raw === null ? Number.NaN : Number(raw);
    if (!Number.isSafeInteger(cursor) || cursor < 0) {
      throw new ProtocolClientError(
        "invalid-response",
        `${label} response is missing a valid event cursor`,
      );
    }
    return cursor;
  }

  async sessions(): Promise<readonly ProtocolSession[]> {
    let result;
    try {
      result = await this.#client.GET("/v1/sessions");
    } catch {
      throw new ProtocolClientError("request-failed", "session request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "session request failed");
    }
    return validateResponse<readonly ProtocolSession[]>(
      "Session[]",
      result.data,
      (loaded) => loaded.sessions,
    );
  }

  async createSession(request: ProtocolCreateSessionRequest): Promise<ProtocolSession> {
    let result;
    try {
      result = await this.#client.POST("/v1/sessions", {
        headers: this.#mutationHeaders(),
        body: request,
      });
    } catch {
      throw new ProtocolClientError("request-failed", "create session request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "create session request failed");
    }
    return validateResponse<ProtocolSession>(
      "Session",
      result.data,
      (loaded) => loaded.session,
    );
  }

  generateSessionTitle(
    prompt: string,
    options: { readonly signal?: AbortSignal } = {},
  ): Promise<ProtocolGeneratedSessionTitle> {
    return this.#validatedMutation(
      "/v1/session-title",
      "generate session title",
      "POST",
      "GeneratedSessionTitle",
      (loaded) => loaded.generatedSessionTitle,
      { prompt },
      options.signal,
    );
  }

  async updateSession(
    sessionId: string,
    request: ProtocolUpdateSessionRequest,
  ): Promise<ProtocolSession> {
    let result;
    try {
      result = await this.#client.PATCH("/v1/sessions/{id}", {
        params: { path: { id: sessionId } },
        headers: this.#mutationHeaders(),
        body: request,
      });
    } catch {
      throw new ProtocolClientError("request-failed", "update session request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "update session request failed");
    }
    return validateResponse<ProtocolSession>(
      "Session",
      result.data,
      (loaded) => loaded.session,
    );
  }

  async deleteSession(sessionId: string): Promise<void> {
    let result;
    try {
      result = await this.#client.DELETE("/v1/sessions/{id}", {
        params: { path: { id: sessionId } },
        headers: this.#mutationHeaders(),
      });
    } catch {
      throw new ProtocolClientError("request-failed", "delete session request failed");
    }
    if (!result.response.ok) {
      throw new ProtocolClientError("request-failed", "delete session request failed");
    }
  }

  async serverInfo(): Promise<ProtocolServerInfo> {
    let result;
    try {
      result = await this.#client.GET("/v1/info");
    } catch {
      throw new ProtocolClientError("request-failed", "server info request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "server info request failed");
    }
    const info = await validateResponse<ProtocolServerInfo>(
      "ServerInfo",
      result.data,
      (loaded) => loaded.serverInfo,
    );
    assertProtocolCompatibility(info.protocol_version);
    return info;
  }

  async sessionSummaries(): Promise<ProtocolSessionSummariesSnapshot> {
    let result;
    try {
      result = await this.#client.GET("/v1/session-summaries");
    } catch {
      throw new ProtocolClientError("request-failed", "session summary request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError(
        "request-failed",
        "session summary request failed",
        result.response.status,
      );
    }
    return validateResponse<ProtocolSessionSummariesSnapshot>(
      "SessionSummariesSnapshot",
      result.data,
      (loaded) => loaded.summaries,
    );
  }

  serverProjectionSnapshot(): Promise<
    ProtocolCursorSnapshot<ProtocolServerProjection>
  > {
    return this.#validatedCursorJson(
      "/v1/server-projection",
      "server projection",
      "ServerProjection",
      (loaded) => loaded.serverProjection,
    );
  }

  async workspaces(): Promise<readonly ProtocolWorkspace[]> {
    let result;
    try {
      result = await this.#client.GET("/v1/workspaces");
    } catch {
      throw new ProtocolClientError("request-failed", "workspace request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "workspace request failed");
    }
    return validateResponse<readonly ProtocolWorkspace[]>(
      "Workspace[]",
      result.data,
      (loaded) => loaded.workspaces,
    );
  }

  registerWorkspace(
    request: ProtocolRegisterWorkspaceRequest,
  ): Promise<ProtocolWorkspace> {
    return this.#validatedMutation(
      "/v1/workspaces",
      "register workspace",
      "POST",
      "Workspace",
      (loaded) => loaded.workspace,
      request,
    );
  }

  async closeWorkspace(workspaceId: string): Promise<void> {
    await this.#mutation(
      `/v1/workspaces/${encodeURIComponent(workspaceId)}`,
      "close workspace",
      "DELETE",
    );
  }

  workspaceBranches(workspaceId: string): Promise<ProtocolBranchList> {
    return this.#validatedJson(
      `/v1/workspaces/${encodeURIComponent(workspaceId)}/branches`,
      "workspace branches",
      "BranchList",
      (loaded) => loaded.branchList,
    );
  }

  async refreshGithubPrs(force = false): Promise<void> {
    const query = force ? "?force=true" : "";
    await this.#mutation(`/v1/github/prs/refresh${query}`, "refresh pull requests", "POST");
  }

  createSessionPr(
    sessionId: string,
    request: ProtocolCreatePrRequest,
  ): Promise<ProtocolPrInfo> {
    return this.#validatedMutation(
      `/v1/sessions/${encodeURIComponent(sessionId)}/pr`,
      "create pull request",
      "POST",
      "PrInfo",
      (loaded) => loaded.prInfo,
      request,
    );
  }

  async sessionPrDetail(
    sessionId: string,
    number: number,
    section?: ProtocolPrDetailSection,
  ): Promise<ProtocolPrDetail> {
    let response: Response;
    try {
      const url = new URL(
        `/v1/sessions/${encodeURIComponent(sessionId)}/prs/${number}`,
        this.#baseUrl,
      );
      if (section !== undefined) url.searchParams.set("section", section);
      response = await this.#fetch(url);
    } catch {
      throw new ProtocolClientError("request-failed", "pull request detail request failed");
    }
    if (!response.ok) {
      let code: string | undefined;
      let message = "pull request detail request failed";
      try {
        const error: unknown = await response.json();
        if (typeof error === "object" && error !== null) {
          const record = error as Record<string, unknown>;
          if (typeof record["code"] === "string") {
            code = record["code"].slice(0, MAX_PROTOCOL_ERROR_FIELD_LENGTH);
          }
          if (typeof record["message"] === "string") {
            const candidate = record["message"].trim();
            if (candidate !== "") {
              message = candidate.slice(0, MAX_PROTOCOL_ERROR_FIELD_LENGTH);
            }
          }
        }
      } catch {
        // Preserve the bounded generic message for malformed error responses.
      }
      throw new ProtocolClientError(
        "request-failed",
        message,
        response.status,
        code,
      );
    }
    return this.#parsePrDetail(response);
  }

  async sessionPrFileDiff(
    sessionId: string,
    number: number,
    path: string,
  ): Promise<ProtocolPrFileDiff> {
    const url = new URL(
      `/v1/sessions/${encodeURIComponent(sessionId)}/prs/${number}/file`,
      this.#baseUrl,
    );
    url.searchParams.set("path", path);
    let response: Response;
    try {
      response = await this.#fetch(url);
    } catch {
      throw new ProtocolClientError("request-failed", "pull request file request failed");
    }
    if (!response.ok) {
      throw new ProtocolClientError(
        "request-failed",
        "pull request file request failed",
        response.status,
      );
    }
    return this.#parsePrFileDiff(response);
  }

  async actOnSessionPr(
    sessionId: string,
    number: number,
    action: ProtocolPrActionRequest,
  ): Promise<ProtocolPrDetail> {
    const response = await this.#mutation(
      `/v1/sessions/${encodeURIComponent(sessionId)}/prs/${number}/actions`,
      "update pull request",
      "POST",
      action,
    );
    return this.#parsePrDetail(response);
  }

  async personas(workspaceId?: string): Promise<readonly ProtocolAgentPersona[]> {
    let result;
    try {
      result = await this.#client.GET("/v1/personas", {
        params: { query: workspaceId === undefined ? {} : { workspace_id: workspaceId } },
      });
    } catch {
      throw new ProtocolClientError("request-failed", "persona request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "persona request failed");
    }
    return validateResponse<readonly ProtocolAgentPersona[]>(
      "AgentPersona[]",
      result.data,
      (loaded) => loaded.personas,
    );
  }

  personaInfos(workspaceId?: string): Promise<readonly ProtocolPersonaInfo[]> {
    const parameters = new URLSearchParams();
    if (workspaceId !== undefined) parameters.set("workspace_id", workspaceId);
    const suffix = parameters.size === 0 ? "" : `?${parameters.toString()}`;
    return this.#validatedJson(
      `/v1/persona-infos${suffix}`,
      "persona information",
      "PersonaInfo[]",
      (loaded) => loaded.personaInfos,
    );
  }

  async upsertPersona(personaId: string, request: ProtocolUpsertPersonaRequest): Promise<void> {
    await this.#mutation(
      `/v1/personas/${encodeURIComponent(personaId)}`,
      "save persona",
      "PUT",
      request,
    );
  }

  async deletePersona(personaId: string): Promise<void> {
    await this.#mutation(
      `/v1/personas/${encodeURIComponent(personaId)}`,
      "reset persona",
      "DELETE",
    );
  }

  async setGlobalDefaults(request: ProtocolSetGlobalDefaultsRequest): Promise<void> {
    await this.#mutation(
      "/v1/config/defaults",
      "set global defaults",
      "PUT",
      request,
    );
  }

  async setDefaultModel(request: ProtocolSetDefaultModelRequest): Promise<void> {
    await this.#mutation(
      "/v1/config/default-model",
      "set default model",
      "PUT",
      request,
    );
  }

  async setDefaultPermissionMode(
    request: ProtocolSetDefaultPermissionModeRequest,
  ): Promise<void> {
    await this.#mutation(
      "/v1/config/default-permission-mode",
      "set default permission mode",
      "PUT",
      request,
    );
  }

  async models(): Promise<readonly ProtocolModelInfo[]> {
    let result;
    try {
      result = await this.#client.GET("/v1/models");
    } catch {
      throw new ProtocolClientError("request-failed", "model request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "model request failed");
    }
    return validateResponse<readonly ProtocolModelInfo[]>(
      "ModelInfo[]",
      result.data,
      (loaded) => loaded.models,
    );
  }

  async refreshModels(): Promise<readonly ProtocolModelInfo[]> {
    let result;
    try {
      result = await this.#client.GET("/v1/models/refresh");
    } catch {
      throw new ProtocolClientError(
        "request-failed",
        "live model refresh failed",
      );
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError(
        "request-failed",
        "live model refresh failed",
      );
    }
    return validateResponse<readonly ProtocolModelInfo[]>(
      "ModelInfo[]",
      result.data,
      (loaded) => loaded.models,
    );
  }

  providers(): Promise<ProtocolProvidersResponse> {
    return this.#validatedJson(
      "/v1/providers",
      "provider",
      "ProvidersResponse",
      (loaded) => loaded.providers,
    );
  }

  knownProviders(): Promise<readonly ProtocolKnownProvider[]> {
    return this.#validatedJson(
      "/v1/providers/known",
      "known provider",
      "KnownProvider[]",
      (loaded) => loaded.knownProviders,
    );
  }

  subscriptionHealth(): Promise<readonly ProtocolSubscriptionHealth[]> {
    return this.#validatedJson(
      "/v1/subscriptions",
      "subscription health",
      "SubscriptionHealth[]",
      (loaded) => loaded.subscriptions,
    );
  }

  upsertProvider(
    providerId: string,
    request: ProtocolUpsertProviderRequest,
  ): Promise<ProtocolProviderInfo> {
    return this.#validatedMutation(
      `/v1/providers/${encodeURIComponent(providerId)}`,
      "save provider",
      "PUT",
      "ProviderInfo",
      (loaded) => loaded.provider,
      request,
    );
  }

  async deleteProvider(providerId: string): Promise<void> {
    await this.#mutation(
      `/v1/providers/${encodeURIComponent(providerId)}`,
      "delete provider",
      "DELETE",
    );
  }

  startProviderLogin(providerId: string): Promise<ProtocolLoginStarted> {
    return this.#validatedMutation(
      `/v1/providers/${encodeURIComponent(providerId)}/login`,
      "start provider login",
      "POST",
      "LoginStarted",
      (loaded) => loaded.loginStarted,
    );
  }

  providerLoginStatus(providerId: string): Promise<ProtocolLoginStatus> {
    return this.#validatedJson(
      `/v1/providers/${encodeURIComponent(providerId)}/login`,
      "provider login status",
      "LoginStatus",
      (loaded) => loaded.loginStatus,
    );
  }

  completeProviderLogin(
    providerId: string,
    callbackUrl: string,
  ): Promise<ProtocolLoginStatus> {
    return this.#validatedMutation(
      `/v1/providers/${encodeURIComponent(providerId)}/login/callback`,
      "complete provider login",
      "POST",
      "LoginStatus",
      (loaded) => loaded.loginStatus,
      { callback_url: callbackUrl },
    );
  }

  localStatus(): Promise<ProtocolLocalStatus> {
    return this.#validatedJson(
      "/v1/local",
      "local model status",
      "LocalStatus",
      (loaded) => loaded.localStatus,
    );
  }

  searchLocalModels(query: string): Promise<readonly ProtocolLocalSearchResult[]> {
    const parameters = new URLSearchParams({ q: query });
    return this.#validatedJson(
      `/v1/local/search?${parameters.toString()}`,
      "local model search",
      "LocalSearchResult[]",
      (loaded) => loaded.localSearch,
    );
  }

  async setLocalEnabled(enabled: boolean): Promise<void> {
    await this.#mutation("/v1/local/enabled", "set local models enabled", "PUT", {
      enabled,
    });
  }

  async addLocalModel(request: ProtocolAddLocalModelRequest): Promise<void> {
    await this.#mutation("/v1/local/models", "add local model", "POST", request);
  }

  async startLocalModelDownload(modelId: string): Promise<void> {
    await this.#mutation(
      `/v1/local/models/${encodeURIComponent(modelId)}/download`,
      "start local model download",
      "POST",
    );
  }

  async cancelLocalModelDownload(modelId: string): Promise<void> {
    await this.#mutation(
      `/v1/local/models/${encodeURIComponent(modelId)}/download`,
      "cancel local model download",
      "DELETE",
    );
  }

  async deleteLocalModel(modelId: string): Promise<void> {
    await this.#mutation(
      `/v1/local/models/${encodeURIComponent(modelId)}`,
      "delete local model",
      "DELETE",
    );
  }

  async restartLocalServer(): Promise<void> {
    await this.#mutation("/v1/local/server/restart", "restart local server", "POST");
  }

  async stopLocalServer(): Promise<void> {
    await this.#mutation("/v1/local/server/stop", "stop local server", "POST");
  }

  automations(): Promise<readonly ProtocolAutomation[]> {
    return this.#validatedJson(
      "/v1/automations",
      "automation",
      "Automation[]",
      (loaded) => loaded.automations,
    );
  }

  automationTemplates(): Promise<readonly ProtocolAutomationTemplate[]> {
    return this.#validatedJson(
      "/v1/automations/templates",
      "automation template",
      "AutomationTemplate[]",
      (loaded) => loaded.automationTemplates,
    );
  }

  createAutomation(
    request: ProtocolUpsertAutomationRequest,
  ): Promise<ProtocolAutomation> {
    return this.#validatedMutation(
      "/v1/automations",
      "create automation",
      "POST",
      "Automation",
      (loaded) => loaded.automation,
      request,
    );
  }

  updateAutomation(
    automationId: string,
    request: ProtocolUpsertAutomationRequest,
  ): Promise<ProtocolAutomation> {
    return this.#validatedMutation(
      `/v1/automations/${encodeURIComponent(automationId)}`,
      "update automation",
      "PUT",
      "Automation",
      (loaded) => loaded.automation,
      request,
    );
  }

  async deleteAutomation(automationId: string): Promise<void> {
    await this.#mutation(
      `/v1/automations/${encodeURIComponent(automationId)}`,
      "delete automation",
      "DELETE",
    );
  }

  async runAutomation(automationId: string): Promise<void> {
    await this.#mutation(
      `/v1/automations/${encodeURIComponent(automationId)}/run`,
      "run automation",
      "POST",
    );
  }

  codeReviewDashboard(): Promise<ProtocolCodeReviewDashboard> {
    return this.#validatedJson(
      "/v1/code-review",
      "code review dashboard",
      "CodeReviewDashboard",
      (loaded) => loaded.codeReviewDashboard,
    );
  }

  async refreshCodeReviews(): Promise<void> {
    await this.#mutation("/v1/code-review/refresh", "refresh code reviews", "POST");
  }

  retryCodeReviewJob(jobId: string): Promise<ProtocolCodeReviewJob> {
    return this.#mutateCodeReviewJob(jobId, "retry", "retry code review");
  }

  retryCodeReviewFinalEditor(jobId: string): Promise<ProtocolCodeReviewJob> {
    return this.#mutateCodeReviewJob(
      jobId,
      "final-editor/retry",
      "retry final review editor",
    );
  }

  cancelCodeReviewJob(jobId: string): Promise<ProtocolCodeReviewJob> {
    return this.#mutateCodeReviewJob(jobId, "cancel", "cancel code review");
  }

  #mutateCodeReviewJob(
    jobId: string,
    action: string,
    description: string,
  ): Promise<ProtocolCodeReviewJob> {
    return this.#validatedMutation(
      `/v1/code-review/jobs/${encodeURIComponent(jobId)}/${action}`,
      description,
      "POST",
      "CodeReviewJob",
      (loaded) => loaded.codeReviewJob,
    );
  }

  codeReviewSettings(): Promise<ProtocolCodeReviewSettings> {
    return this.#validatedJson(
      "/v1/config/code-review",
      "code review settings",
      "CodeReviewSettings",
      (loaded) => loaded.codeReviewSettings,
    );
  }

  setCodeReviewSettings(
    request: ProtocolSetCodeReviewSettingsRequest,
  ): Promise<ProtocolCodeReviewSettings> {
    return this.#validatedMutation(
      "/v1/config/code-review",
      "save code review settings",
      "PUT",
      "CodeReviewSettings",
      (loaded) => loaded.codeReviewSettings,
      request,
    );
  }

  configureCodeReviewGithubApp(
    request: ProtocolConfigureGithubAppRequest,
  ): Promise<ProtocolGithubAppStatus> {
    return this.#validatedMutation(
      "/v1/code-review/github-app",
      "configure code review GitHub App",
      "PUT",
      "GithubAppStatus",
      (loaded) => loaded.githubAppStatus,
      request,
    );
  }

  updateCodeReviewRepository(
    request: ProtocolUpdateCodeReviewRepositoryRequest,
  ): Promise<ProtocolCodeReviewRepository> {
    return this.#validatedMutation(
      "/v1/code-review/repository",
      "save code review repository",
      "PUT",
      "CodeReviewRepository",
      (loaded) => loaded.codeReviewRepository,
      request,
    );
  }

  skillsSettings(): Promise<ProtocolSkillsSettings> {
    return this.#validatedJson(
      "/v1/config/skills",
      "skill settings",
      "SkillsSettings",
      (loaded) => loaded.skillsSettings,
    );
  }

  async setSkillsSettings(request: ProtocolSetSkillsSettingsRequest): Promise<void> {
    await this.#mutation("/v1/config/skills", "save skill settings", "PUT", request);
  }

  gitWorktreeSettings(): Promise<ProtocolGitWorktreeSettings> {
    return this.#validatedJson(
      "/v1/config/git-worktrees",
      "Session naming settings",
      "GitWorktreeSettings",
      (loaded) => loaded.gitWorktreeSettings,
    );
  }

  gitWorktreeSettingsSnapshot(): Promise<
    ProtocolCursorSnapshot<ProtocolGitWorktreeSettings>
  > {
    return this.#validatedCursorJson(
      "/v1/config/git-worktrees",
      "Session naming settings",
      "GitWorktreeSettings",
      (loaded) => loaded.gitWorktreeSettings,
    );
  }

  setGitWorktreeSettings(
    request: ProtocolSetGitWorktreeSettingsRequest,
  ): Promise<ProtocolGitWorktreeSettings> {
    return this.#validatedMutation(
      "/v1/config/git-worktrees",
      "save session naming settings",
      "PUT",
      "GitWorktreeSettings",
      (loaded) => loaded.gitWorktreeSettings,
      request,
    );
  }

  setGitWorktreeSettingsSnapshot(
    request: ProtocolSetGitWorktreeSettingsRequest,
  ): Promise<ProtocolCursorSnapshot<ProtocolGitWorktreeSettings>> {
    return this.#validatedCursorMutation(
      "/v1/config/git-worktrees",
      "save session naming settings",
      "PUT",
      "GitWorktreeSettings",
      (loaded) => loaded.gitWorktreeSettings,
      request,
    );
  }

  async installTitleModel(): Promise<void> {
    await this.#mutation(
      "/v1/config/git-worktrees/title-model/install",
      "install title model",
      "POST",
    );
  }

  async cancelTitleModelInstall(): Promise<void> {
    await this.#mutation(
      "/v1/config/git-worktrees/title-model/install",
      "cancel title model install",
      "DELETE",
    );
  }

  githubIntegration(): Promise<ProtocolGithubIntegration> {
    return this.#validatedJson(
      "/v1/integrations/github",
      "GitHub integration",
      "GithubIntegration",
      (loaded) => loaded.githubIntegration,
    );
  }

  addGithubHost(
    request: ProtocolAddGithubHostRequest,
  ): Promise<ProtocolGithubIntegration> {
    return this.#validatedMutation(
      "/v1/integrations/github/hosts",
      "add GitHub host",
      "POST",
      "GithubIntegration",
      (loaded) => loaded.githubIntegration,
      request,
    );
  }

  removeGithubHost(host: string): Promise<ProtocolGithubIntegration> {
    return this.#validatedMutation(
      `/v1/integrations/github/hosts/${encodeURIComponent(host)}`,
      "remove GitHub host",
      "DELETE",
      "GithubIntegration",
      (loaded) => loaded.githubIntegration,
    );
  }

  mcpServers(
    workspaceId?: string,
    probe = true,
  ): Promise<readonly ProtocolMcpServerInfo[]> {
    const parameters = new URLSearchParams({ probe: String(probe) });
    if (workspaceId !== undefined) parameters.set("workspace_id", workspaceId);
    return this.#validatedJson(
      `/v1/mcp-servers?${parameters.toString()}`,
      "MCP server",
      "McpServerInfo[]",
      (loaded) => loaded.mcpServers,
    );
  }

  /** Effective app/workspace/branch MCP configuration seen by this session. */
  sessionMcpServers(
    sessionId: string,
  ): Promise<readonly ProtocolMcpServerInfo[]> {
    return this.#validatedJson(
      `/v1/sessions/${encodeURIComponent(sessionId)}/mcp-servers`,
      "session MCP server",
      "McpServerInfo[]",
      (loaded) => loaded.mcpServers,
    );
  }

  async upsertMcpServer(
    name: string,
    request: ProtocolUpsertMcpServerRequest,
  ): Promise<void> {
    await this.#mutation(
      `/v1/mcp-servers/${encodeURIComponent(name)}`,
      "save MCP server",
      "PUT",
      request,
    );
  }

  async setMcpServerEnabled(
    name: string,
    request: ProtocolSetMcpServerEnabledRequest,
  ): Promise<void> {
    await this.#mutation(
      `/v1/mcp-servers/${encodeURIComponent(name)}/enabled`,
      "update MCP server enablement",
      "PUT",
      request,
    );
  }

  async deleteMcpServer(
    name: string,
    scope: string,
    workspaceId?: string,
  ): Promise<void> {
    const parameters = new URLSearchParams({ scope });
    if (workspaceId !== undefined) parameters.set("workspace_id", workspaceId);
    await this.#mutation(
      `/v1/mcp-servers/${encodeURIComponent(name)}?${parameters.toString()}`,
      "delete MCP server",
      "DELETE",
    );
  }

  mcpServerLogs(name: string): Promise<ProtocolMcpLogs> {
    return this.#validatedJson(
      `/v1/mcp-servers/${encodeURIComponent(name)}/logs`,
      "MCP server logs",
      "McpLogs",
      (loaded) => loaded.mcpLogs,
    );
  }

  clis(): Promise<ProtocolCliList> {
    return this.#validatedJson(
      "/v1/clis",
      "CLI list",
      "CliList",
      (loaded) => loaded.cliList,
    );
  }

  cliInstallStatus(cliId: string): Promise<ProtocolCliInstallStatus> {
    return this.#validatedJson(
      `/v1/clis/${encodeURIComponent(cliId)}/install`,
      "CLI install status",
      "CliInstallStatus",
      (loaded) => loaded.cliInstallStatus,
    );
  }

  async startCliInstall(cliId: string): Promise<void> {
    await this.#mutation(
      `/v1/clis/${encodeURIComponent(cliId)}/install`,
      "start CLI install",
      "POST",
    );
  }

  async cancelCliInstall(cliId: string): Promise<void> {
    await this.#mutation(
      `/v1/clis/${encodeURIComponent(cliId)}/install`,
      "cancel CLI install",
      "DELETE",
    );
  }

  async uninstallCli(cliId: string): Promise<void> {
    await this.#mutation(
      `/v1/clis/${encodeURIComponent(cliId)}`,
      "uninstall CLI",
      "DELETE",
    );
  }

  async threads(sessionId: string): Promise<readonly ProtocolThread[]> {
    let result;
    try {
      result = await this.#client.GET("/v1/threads", {
        params: { query: { session_id: sessionId } },
      });
    } catch {
      throw new ProtocolClientError("request-failed", "thread request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "thread request failed");
    }
    return validateResponse<readonly ProtocolThread[]>(
      "Thread[]",
      result.data,
      (loaded) => loaded.threads,
    );
  }

  threadSubagents(
    threadId: string,
    recursive = false,
  ): Promise<readonly ProtocolThread[]> {
    const query = recursive ? "?recursive=true" : "";
    return this.#validatedJson(
      `/v1/threads/${encodeURIComponent(threadId)}/subagents${query}`,
      "thread subagent",
      "Thread[]",
      (loaded) => loaded.threads,
    );
  }

  async threadStatuses(sessionId: string): Promise<readonly ProtocolThreadStatus[]> {
    let result;
    try {
      result = await this.#client.GET("/v1/thread-statuses", {
        params: { query: { session_id: sessionId } },
      });
    } catch {
      throw new ProtocolClientError("request-failed", "thread status request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "thread status request failed");
    }
    return validateResponse<readonly ProtocolThreadStatus[]>(
      "ThreadStatus[]",
      result.data,
      (loaded) => loaded.threadStatuses,
    );
  }

  /** Seed a thread from its bounded folded tail before following live SSE.
   * The response cursor is the exact snapshot/stream handoff boundary. */
  async threadView(
    threadId: string,
    before?: number,
    options: { readonly signal?: AbortSignal } = {},
  ): Promise<ProtocolCursorSnapshot<ProtocolThreadViewSnapshot>> {
    const { threadView } = await import("../generated/thread-view-validator.js");
    const query = new URLSearchParams({
      limit: String(THREAD_VIEW_PAGE_SIZE),
      turn_aligned: "true",
    });
    if (before !== undefined) query.set("before", String(before));
    return this.#validatedCursorJson(
      `/v1/threads/${encodeURIComponent(threadId)}/view?${query.toString()}`,
      "thread view",
      "ThreadViewSnapshot",
      () => threadView,
      options.signal,
    );
  }

  /** Fetch heavyweight arguments/results only when historical tool detail is
   * explicitly opened. Collapsed transcript pages intentionally omit them. */
  async threadToolDetails(
    threadId: string,
    callId: string,
  ): Promise<ProtocolThreadToolDetails> {
    let response: Response;
    try {
      response = await this.#fetch(new URL(
        `/v1/threads/${encodeURIComponent(threadId)}/tools/${encodeURIComponent(callId)}`,
        this.#baseUrl,
      ));
    } catch {
      throw new ProtocolClientError("request-failed", "tool detail request failed");
    }
    if (!response.ok) {
      throw new ProtocolClientError(
        "request-failed",
        "tool detail request failed",
        response.status,
      );
    }
    let value: unknown;
    try {
      value = await response.json();
    } catch {
      throw new ProtocolClientError(
        "invalid-response",
        "server returned invalid ThreadToolDetails",
      );
    }
    if (!isThreadToolDetails(value) || value.call_id !== callId) {
      throw new ProtocolClientError(
        "invalid-response",
        "server returned invalid ThreadToolDetails",
      );
    }
    return value;
  }

  async createThread(request: ProtocolCreateThreadRequest): Promise<ProtocolThread> {
    let result;
    try {
      result = await this.#client.POST("/v1/threads", {
        headers: this.#mutationHeaders(),
        body: request,
      });
    } catch {
      throw new ProtocolClientError("request-failed", "create thread request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "create thread request failed");
    }
    return validateResponse<ProtocolThread>(
      "Thread",
      result.data,
      (loaded) => loaded.thread,
    );
  }

  async updateThread(
    threadId: string,
    request: ProtocolUpdateThreadRequest,
  ): Promise<ProtocolThread> {
    let result;
    try {
      result = await this.#client.PATCH("/v1/threads/{id}", {
        params: { path: { id: threadId } },
        headers: this.#mutationHeaders(),
        body: request,
      });
    } catch {
      throw new ProtocolClientError("request-failed", "update thread request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      const error = result.error;
      throw new ProtocolClientError(
        "request-failed",
        error?.message ?? "update thread request failed",
        result.response.status,
        error?.code,
      );
    }
    return validateResponse<ProtocolThread>(
      "Thread",
      result.data,
      (loaded) => loaded.thread,
    );
  }

  async updateQueuedPrompt(
    promptId: string,
    request: ProtocolUpdateQueuedPromptRequest,
  ): Promise<void> {
    let result;
    try {
      result = await this.#client.PATCH("/v1/queue/{id}", {
        params: { path: { id: promptId } },
        headers: this.#mutationHeaders(),
        body: request,
      });
    } catch {
      throw new ProtocolClientError("request-failed", "update queued prompt request failed");
    }
    if (!result.response.ok) {
      throw new ProtocolClientError("request-failed", "update queued prompt request failed");
    }
  }

  async listQueue(threadId: string): Promise<readonly ProtocolQueuedPrompt[]> {
    let result;
    try {
      result = await this.#client.GET("/v1/threads/{id}/queue", {
        params: { path: { id: threadId } },
      });
    } catch {
      throw new ProtocolClientError("request-failed", "list queue request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "list queue request failed");
    }
    return validateResponse<readonly ProtocolQueuedPrompt[]>(
      "QueuedPrompt[]",
      result.data,
      (loaded) => loaded.queuedPrompts,
    );
  }

  async deleteQueuedPrompt(promptId: string): Promise<void> {
    let result;
    try {
      result = await this.#client.DELETE("/v1/queue/{id}", {
        params: { path: { id: promptId } },
        headers: this.#mutationHeaders(),
      });
    } catch {
      throw new ProtocolClientError("request-failed", "delete queued prompt request failed");
    }
    if (!result.response.ok) {
      throw new ProtocolClientError("request-failed", "delete queued prompt request failed");
    }
  }

  async reorderQueue(
    threadId: string,
    ids: readonly string[],
  ): Promise<readonly ProtocolQueuedPrompt[]> {
    let result;
    try {
      result = await this.#client.PUT("/v1/threads/{id}/queue", {
        params: { path: { id: threadId } },
        headers: this.#mutationHeaders(),
        body: { ids: [...ids] },
      });
    } catch {
      throw new ProtocolClientError("request-failed", "reorder queue request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "reorder queue request failed");
    }
    return validateResponse<readonly ProtocolQueuedPrompt[]>(
      "QueuedPrompt[]",
      result.data,
      (loaded) => loaded.queuedPrompts,
    );
  }

  async dispatchQueue(threadId: string): Promise<ProtocolTurnAccepted> {
    let result;
    try {
      result = await this.#client.POST("/v1/threads/{id}/queue/dispatch", {
        params: { path: { id: threadId } },
        headers: this.#mutationHeaders(),
      });
    } catch {
      throw new ProtocolClientError("request-failed", "dispatch queue request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "dispatch queue request failed");
    }
    return validateResponse<ProtocolTurnAccepted>(
      "TurnAccepted",
      result.data,
      (loaded) => loaded.turnAccepted,
    );
  }

  async dispatchQueuedPrompt(promptId: string): Promise<ProtocolTurnAccepted> {
    let result;
    try {
      result = await this.#client.POST("/v1/queue/{id}/dispatch", {
        params: { path: { id: promptId } },
        headers: this.#mutationHeaders(),
      });
    } catch {
      throw new ProtocolClientError(
        "request-failed",
        "dispatch queued prompt request failed",
      );
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError(
        "request-failed",
        "dispatch queued prompt request failed",
      );
    }
    return validateResponse<ProtocolTurnAccepted>(
      "TurnAccepted",
      result.data,
      (loaded) => loaded.turnAccepted,
    );
  }

  async sendMessage(
    threadId: string,
    request: ProtocolSendMessageRequest,
  ): Promise<ProtocolTurnAccepted> {
    let result;
    try {
      result = await this.#client.POST("/v1/threads/{id}/messages", {
        params: { path: { id: threadId } },
        headers: this.#mutationHeaders(),
        body: request,
      });
    } catch {
      throw new ProtocolClientError("request-failed", "message request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "message request failed");
    }
    return validateResponse<ProtocolTurnAccepted>(
      "TurnAccepted",
      result.data,
      (loaded) => loaded.turnAccepted,
    );
  }

  async executeCommand(
    threadId: string,
    request: ProtocolExecuteCommandRequest,
  ): Promise<ProtocolCommandResult> {
    return this.#validatedMutation(
      `/v1/threads/${encodeURIComponent(threadId)}/commands`,
      "execute command",
      "POST",
      "CommandResult",
      (loaded) => loaded.commandResult,
      request,
    );
  }

  async steerTurn(
    threadId: string,
    request: ProtocolSteerTurnRequest,
  ): Promise<void> {
    let result;
    try {
      result = await this.#client.POST("/v1/threads/{id}/steer", {
        params: { path: { id: threadId } },
        headers: this.#mutationHeaders(),
        body: request,
      });
    } catch {
      throw new ProtocolClientError("request-failed", "steer request failed");
    }
    if (!result.response.ok) {
      throw new ProtocolClientError("request-failed", "steer request failed");
    }
  }

  async cancelTurn(threadId: string): Promise<void> {
    let result;
    try {
      result = await this.#client.POST("/v1/threads/{id}/cancel", {
        params: { path: { id: threadId } },
        headers: this.#mutationHeaders(),
      });
    } catch {
      throw new ProtocolClientError("request-failed", "cancel request failed");
    }
    if (!result.response.ok) {
      throw new ProtocolClientError("request-failed", "cancel request failed");
    }
  }

  async resolveApproval(request: ProtocolResolveApprovalRequest): Promise<void> {
    let result;
    try {
      result = await this.#client.POST("/v1/approvals", {
        headers: this.#mutationHeaders(),
        body: request,
      });
    } catch {
      throw new ProtocolClientError("request-failed", "approval request failed");
    }
    if (!result.response.ok) {
      throw new ProtocolClientError("request-failed", "approval request failed");
    }
  }

  async resolveQuestion(request: ProtocolResolveQuestionRequest): Promise<void> {
    let result;
    try {
      result = await this.#client.POST("/v1/questions", {
        headers: this.#mutationHeaders(),
        body: request,
      });
    } catch {
      throw new ProtocolClientError("request-failed", "question request failed");
    }
    if (!result.response.ok) {
      throw new ProtocolClientError("request-failed", "question request failed");
    }
  }

  async sessionDiffSummary(sessionId: string): Promise<ProtocolSessionDiffSummary> {
    let result;
    try {
      result = await this.#client.GET("/v1/sessions/{id}/diff/summary", {
        params: { path: { id: sessionId } },
      });
    } catch {
      throw new ProtocolClientError(
        "request-failed",
        "session diff summary request failed",
      );
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError(
        "request-failed",
        "session diff summary request failed",
        result.response.status,
      );
    }
    if (!isSessionDiffSummary(result.data)) {
      throw new ProtocolClientError(
        "invalid-response",
        "server returned invalid SessionDiffSummary",
      );
    }
    return result.data;
  }

  async sessionFileDiff(
    sessionId: string,
    path: string,
  ): Promise<ProtocolSessionFileDiff> {
    let result;
    try {
      result = await this.#client.GET("/v1/sessions/{id}/diff/file", {
        params: { path: { id: sessionId }, query: { path } },
      });
    } catch {
      throw new ProtocolClientError(
        "request-failed",
        "session file diff request failed",
      );
    }
    if (!result.response.ok || result.data === undefined) {
      const tooLarge = result.response.status === 413;
      throw new ProtocolClientError(
        "request-failed",
        tooLarge
          ? "This file's diff is too large to preview."
          : "session file diff request failed",
        result.response.status,
      );
    }
    if (!isSessionFileDiff(result.data)) {
      throw new ProtocolClientError(
        "invalid-response",
        "server returned invalid SessionFileDiff",
      );
    }
    return result.data;
  }

  async sessionUsage(sessionId: string): Promise<ProtocolUsageSummary> {
    let result;
    try {
      result = await this.#client.GET("/v1/sessions/{id}/usage", {
        params: { path: { id: sessionId } },
      });
    } catch {
      throw new ProtocolClientError("request-failed", "session usage request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "session usage request failed");
    }
    return validateResponse<ProtocolUsageSummary>(
      "UsageSummary",
      result.data,
      (loaded) => loaded.usageSummary,
    );
  }

  async threadUsage(threadId: string): Promise<ProtocolUsageSummary> {
    let result;
    try {
      result = await this.#client.GET("/v1/threads/{id}/usage", {
        params: { path: { id: threadId } },
      });
    } catch {
      throw new ProtocolClientError("request-failed", "thread usage request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "thread usage request failed");
    }
    return validateResponse<ProtocolUsageSummary>(
      "UsageSummary",
      result.data,
      (loaded) => loaded.usageSummary,
    );
  }

  async restoreSessionCheckpoint(
    sessionId: string,
    direction: ProtocolRelativeRestoreDirection,
  ): Promise<void> {
    await this.#mutation(
      `/v1/sessions/${encodeURIComponent(sessionId)}/${encodeURIComponent(direction)}`,
      "session checkpoint restore",
      "POST",
    );
  }

  async restoreCheckpoint(checkpointId: string): Promise<void> {
    await this.#mutation(
      `/v1/checkpoints/${encodeURIComponent(checkpointId)}/restore`,
      "checkpoint restore",
      "POST",
    );
  }

  async forkCheckpoint(checkpointId: string): Promise<ProtocolForkCheckpointResponse> {
    let result;
    try {
      result = await this.#client.POST("/v1/checkpoints/{id}/fork", {
        params: { path: { id: checkpointId } },
        headers: this.#mutationHeaders(),
      });
    } catch {
      throw new ProtocolClientError("request-failed", "checkpoint fork request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "checkpoint fork request failed");
    }
    return validateResponse<ProtocolForkCheckpointResponse>(
      "ForkCheckpointResponse",
      result.data,
      (loaded) => loaded.forkCheckpointResponse,
    );
  }

  async sessionFiles(
    sessionId: string,
    path = ".",
  ): Promise<readonly ProtocolDirEntry[]> {
    let result;
    try {
      result = await this.#client.GET("/v1/sessions/{id}/files", {
        params: { path: { id: sessionId }, query: { path } },
      });
    } catch {
      throw new ProtocolClientError("request-failed", "session files request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "session files request failed");
    }
    return validateResponse<readonly ProtocolDirEntry[]>(
      "DirEntry[]",
      result.data,
      (loaded) => loaded.dirEntries,
    );
  }

  async sessionPaths(sessionId: string): Promise<readonly string[]> {
    let result;
    try {
      result = await this.#client.GET("/v1/sessions/{id}/paths", {
        params: { path: { id: sessionId } },
      });
    } catch {
      throw new ProtocolClientError("request-failed", "session paths request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "session paths request failed");
    }
    return validateResponse<readonly string[]>(
      "Path[]",
      result.data,
      (loaded) => loaded.paths,
    );
  }

  async sessionFile(sessionId: string, path: string): Promise<ProtocolFileContent> {
    let result;
    try {
      result = await this.#client.GET("/v1/sessions/{id}/file", {
        params: { path: { id: sessionId }, query: { path } },
      });
    } catch {
      throw new ProtocolClientError("request-failed", "session file request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "session file request failed");
    }
    return validateResponse<ProtocolFileContent>(
      "FileContent",
      result.data,
      (loaded) => loaded.fileContent,
    );
  }

  async terminals(sessionId: string): Promise<readonly ProtocolTerminalInfo[]> {
    let result;
    try {
      result = await this.#client.GET("/v1/sessions/{id}/terminals", {
        params: { path: { id: sessionId } },
      });
    } catch {
      throw new ProtocolClientError("request-failed", "terminal list request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "terminal list request failed");
    }
    return validateResponse<readonly ProtocolTerminalInfo[]>(
      "TerminalInfo[]",
      result.data,
      (loaded) => loaded.terminalInfos,
    );
  }

  openTerminal(
    sessionId: string,
    cols: number,
    rows: number,
  ): Promise<ProtocolTerminalInfo> {
    return this.#startTerminal("/v1/sessions/{id}/terminal", sessionId, cols, rows);
  }

  createTerminal(
    sessionId: string,
    cols: number,
    rows: number,
  ): Promise<ProtocolTerminalInfo> {
    return this.#startTerminal("/v1/sessions/{id}/terminals", sessionId, cols, rows);
  }

  async #startTerminal(
    path: "/v1/sessions/{id}/terminal" | "/v1/sessions/{id}/terminals",
    sessionId: string,
    cols: number,
    rows: number,
  ): Promise<ProtocolTerminalInfo> {
    let result;
    try {
      result = path === "/v1/sessions/{id}/terminal"
        ? await this.#client.POST(path, {
            params: { path: { id: sessionId } },
            headers: this.#mutationHeaders(),
            body: { cols, rows },
          })
        : await this.#client.POST(path, {
            params: { path: { id: sessionId } },
            headers: this.#mutationHeaders(),
            body: { cols, rows },
          });
    } catch {
      throw new ProtocolClientError("request-failed", "open terminal request failed");
    }
    if (!result.response.ok || result.data === undefined) {
      throw new ProtocolClientError("request-failed", "open terminal request failed");
    }
    return validateResponse<ProtocolTerminalInfo>(
      "TerminalInfo",
      result.data,
      (loaded) => loaded.terminalInfo,
    );
  }

  async terminalInput(terminalId: string, data: string): Promise<void> {
    const bytes = new TextEncoder().encode(data);
    let binary = "";
    for (let offset = 0; offset < bytes.length; offset += 8_192) {
      binary += String.fromCharCode(...bytes.subarray(offset, offset + 8_192));
    }
    let result;
    try {
      result = await this.#client.POST("/v1/terminals/{id}/input", {
        params: { path: { id: terminalId } },
        headers: this.#mutationHeaders(),
        body: { data: globalThis.btoa(binary) },
      });
    } catch {
      throw new ProtocolClientError("request-failed", "terminal input request failed");
    }
    if (!result.response.ok) {
      throw new ProtocolClientError("request-failed", "terminal input request failed");
    }
  }

  async terminalResize(terminalId: string, cols: number, rows: number): Promise<void> {
    let result;
    try {
      result = await this.#client.POST("/v1/terminals/{id}/resize", {
        params: { path: { id: terminalId } },
        headers: this.#mutationHeaders(),
        body: { cols, rows },
      });
    } catch {
      throw new ProtocolClientError("request-failed", "terminal resize request failed");
    }
    if (!result.response.ok) {
      throw new ProtocolClientError("request-failed", "terminal resize request failed");
    }
  }

  async killTerminal(terminalId: string): Promise<void> {
    let result;
    try {
      result = await this.#client.DELETE("/v1/terminals/{id}", {
        params: { path: { id: terminalId } },
        headers: this.#mutationHeaders(),
      });
    } catch {
      throw new ProtocolClientError("request-failed", "terminal close request failed");
    }
    if (!result.response.ok) {
      throw new ProtocolClientError("request-failed", "terminal close request failed");
    }
  }

  terminalOutputUrl(terminalId: string): string {
    return new URL(
      `/v1/terminals/${encodeURIComponent(terminalId)}/output`,
      this.#baseUrl,
    ).href;
  }

  async serverEvents(options: {
    readonly after: number;
    readonly onEvent: (event: ProtocolIngressEvent) => void;
    readonly onOpen?: () => void;
    readonly onDiagnostic?: (diagnostic: SafeStreamDiagnostic) => void;
  }): Promise<CursorEventStream<ProtocolIngressEvent>> {
    const parse = await loadProtocolEventParser();
    return new CursorEventStream({
      path: new URL("/v1/events", this.#baseUrl).href,
      origin: this.#baseUrl,
      after: options.after,
      parse,
      onEvent: options.onEvent,
      ...(options.onOpen === undefined ? {} : { onOpen: options.onOpen }),
      ...(options.onDiagnostic === undefined
        ? {}
        : { onDiagnostic: options.onDiagnostic }),
      ...(this.#eventSourceFactory === undefined
        ? {}
        : { eventSourceFactory: this.#eventSourceFactory }),
    });
  }

  async threadEvents(
    threadId: string,
    options: {
      readonly after: number;
      readonly onEvent: (event: ProtocolIngressEvent) => void;
      readonly onOpen?: () => void;
      readonly onDiagnostic?: (diagnostic: SafeStreamDiagnostic) => void;
    },
  ): Promise<CursorEventStream<ProtocolIngressEvent>> {
    const parse = await loadProtocolEventParser();
    return new CursorEventStream({
      path: new URL(
        `/v1/threads/${encodeURIComponent(threadId)}/events`,
        this.#baseUrl,
      ).href,
      origin: this.#baseUrl,
      after: options.after,
      parse,
      onEvent: options.onEvent,
      ...(options.onOpen === undefined ? {} : { onOpen: options.onOpen }),
      ...(options.onDiagnostic === undefined
        ? {}
        : { onDiagnostic: options.onDiagnostic }),
      ...(this.#eventSourceFactory === undefined
        ? {}
        : { eventSourceFactory: this.#eventSourceFactory }),
    });
  }
}
