import type {
  ProtocolAutomation,
  ProtocolAutomationSchedule,
  ProtocolAutomationTemplate,
  ProtocolUpsertAutomationRequest,
} from "../services/protocol-client.js";

export type AutomationScheduleKind = "hourly" | "daily" | "weekly";
export type AutomationPermissionMode = NonNullable<
  ProtocolUpsertAutomationRequest["permission_mode"]
>;

export interface AutomationDraft {
  readonly name: string;
  readonly prompt: string;
  readonly workspaceId: string;
  readonly mode: string;
  readonly model: string;
  readonly permissionMode: AutomationPermissionMode;
  readonly scheduleKind: AutomationScheduleKind;
  readonly minute: string;
  readonly time: string;
  readonly days: readonly number[];
  readonly enabled: boolean;
}

export interface AutomationDraftErrors {
  readonly name?: string;
  readonly prompt?: string;
  readonly workspaceId?: string;
  readonly schedule?: string;
}

const SCHEDULE_KINDS = new Set<AutomationScheduleKind>([
  "hourly",
  "daily",
  "weekly",
]);
const PERMISSION_MODES = new Set<AutomationPermissionMode>([
  "ask",
  "allow_list",
  "yolo",
]);
const TIME_OF_DAY = /^(?:[01]\d|2[0-3]):[0-5]\d$/;

export const AUTOMATION_DAY_NAMES = [
  "Monday",
  "Tuesday",
  "Wednesday",
  "Thursday",
  "Friday",
  "Saturday",
  "Sunday",
] as const;

const scheduleKind = (value: string): AutomationScheduleKind =>
  SCHEDULE_KINDS.has(value as AutomationScheduleKind)
    ? (value as AutomationScheduleKind)
    : "daily";

const permissionMode = (
  value: AutomationPermissionMode | undefined,
): AutomationPermissionMode =>
  value !== undefined && PERMISSION_MODES.has(value) ? value : "ask";

const scheduleDays = (value: readonly number[] | undefined): readonly number[] =>
  [...new Set((value ?? []).filter((day) => Number.isInteger(day) && day >= 0 && day <= 6))]
    .sort((left, right) => left - right);

const scheduleMinute = (value: number | undefined): string =>
  Number.isInteger(value) && value !== undefined && value >= 0 && value <= 59
    ? String(value)
    : "0";

const scheduleTime = (value: string | undefined): string =>
  value !== undefined && TIME_OF_DAY.test(value.trim()) ? value.trim() : "09:00";

const draftForSchedule = (
  schedule: ProtocolAutomationSchedule,
): Pick<AutomationDraft, "scheduleKind" | "minute" | "time" | "days"> => ({
  scheduleKind: scheduleKind(schedule.kind),
  minute: scheduleMinute(schedule.minute),
  time: scheduleTime(schedule.time),
  days: scheduleDays(schedule.days),
});

export const emptyAutomationDraft = (workspaceId = ""): AutomationDraft => ({
  name: "",
  prompt: "",
  workspaceId,
  mode: "",
  model: "",
  permissionMode: "ask",
  scheduleKind: "daily",
  minute: "0",
  time: "09:00",
  days: [],
  enabled: true,
});

export const automationDraftFrom = (
  automation: ProtocolAutomation,
): AutomationDraft => ({
  name: automation.name,
  prompt: automation.prompt,
  workspaceId: automation.workspace_id,
  mode: automation.mode ?? "",
  model: automation.model ?? "",
  permissionMode: permissionMode(automation.permission_mode),
  ...draftForSchedule(automation.schedule),
  enabled: automation.enabled,
});

export const automationDraftFromTemplate = (
  template: ProtocolAutomationTemplate,
  workspaceId: string,
): AutomationDraft => ({
  ...emptyAutomationDraft(workspaceId),
  name: template.name,
  prompt: template.prompt,
  ...draftForSchedule(template.schedule),
});

export const validateAutomationDraft = (
  draft: AutomationDraft,
): AutomationDraftErrors => {
  const errors: {
    name?: string;
    prompt?: string;
    workspaceId?: string;
    schedule?: string;
  } = {};
  if (draft.name.trim() === "") errors.name = "Enter an automation name.";
  if (draft.prompt.trim() === "") errors.prompt = "Enter the prompt to run.";
  if (draft.workspaceId === "") errors.workspaceId = "Choose a workspace.";

  if (draft.scheduleKind === "hourly") {
    const minute = Number(draft.minute);
    if (!Number.isInteger(minute) || minute < 0 || minute > 59) {
      errors.schedule = "Minute must be a whole number from 0 through 59.";
    }
  } else if (!TIME_OF_DAY.test(draft.time.trim())) {
    errors.schedule = "Enter a 24-hour time such as 09:30.";
  } else if (draft.scheduleKind === "weekly" && scheduleDays(draft.days).length === 0) {
    errors.schedule = "Choose at least one day for a weekly schedule.";
  }
  return errors;
};

export const hasAutomationDraftErrors = (errors: AutomationDraftErrors): boolean =>
  Object.values(errors).some((error) => error !== undefined);

const scheduleFromDraft = (draft: AutomationDraft): ProtocolAutomationSchedule => {
  if (draft.scheduleKind === "hourly") {
    return {
      kind: "hourly",
      minute: Number(draft.minute),
      time: "",
      days: [],
    };
  }
  if (draft.scheduleKind === "weekly") {
    return {
      kind: "weekly",
      minute: 0,
      time: draft.time.trim(),
      days: [...scheduleDays(draft.days)],
    };
  }
  return {
    kind: "daily",
    minute: 0,
    time: draft.time.trim(),
    days: [],
  };
};

export const automationRequestFromDraft = (
  draft: AutomationDraft,
): ProtocolUpsertAutomationRequest => {
  const errors = validateAutomationDraft(draft);
  if (hasAutomationDraftErrors(errors)) {
    throw new TypeError(Object.values(errors).find((error) => error !== undefined));
  }
  return {
    name: draft.name.trim(),
    prompt: draft.prompt.trim(),
    workspace_id: draft.workspaceId,
    mode: draft.mode === "" ? null : draft.mode,
    model: draft.model === "" ? null : draft.model,
    permission_mode: draft.permissionMode,
    schedule: scheduleFromDraft(draft),
    enabled: draft.enabled,
  };
};

export const automationScheduleSummary = (
  schedule: ProtocolAutomationSchedule,
): string => {
  const kind = scheduleKind(schedule.kind);
  if (kind === "hourly") {
    const minute = Number.isInteger(schedule.minute) ? (schedule.minute ?? 0) : 0;
    return `Hourly at :${String(Math.min(59, Math.max(0, minute))).padStart(2, "0")}`;
  }
  const time = scheduleTime(schedule.time);
  if (kind === "daily") return `Daily at ${time}`;
  const days = scheduleDays(schedule.days);
  if (days.length === 0) return `Weekly at ${time} (no days selected)`;
  if (days.length === 7) return `Daily at ${time}`;
  const names = days.map((day) => AUTOMATION_DAY_NAMES[day]?.slice(0, 3) ?? "");
  return `${names.join(", ")} at ${time}`;
};
