import type { components as ProtocolComponents } from "../generated/protocol.js";

type Question = ProtocolComponents["schemas"]["Question"];
type QuestionAnswer = ProtocolComponents["schemas"]["QuestionAnswer"];

/** Synthetic choice used by both frontends for the trailing free-form answer. */
export const OTHER_OPTION_ID = "__other__";
export const QUESTION_SKIPPED_STATUS = "Skipped";
export const QUESTION_SKIPPED_MESSAGE = "The questions were skipped.";

export interface QuestionWizardState {
  /** `questions.length` is the review page. */
  readonly step: number;
  readonly selections: readonly (readonly string[])[];
  readonly otherTexts: readonly string[];
}

export interface QuestionAnswerSummary {
  readonly prompt: string;
  readonly answer: string;
}

export const createQuestionWizard = (questionCount: number): QuestionWizardState => ({
  step: 0,
  selections: Array.from({ length: Math.max(0, questionCount) }, () => []),
  otherTexts: Array.from({ length: Math.max(0, questionCount) }, () => ""),
});

/** Preserve answers while making stale/incomplete state safe for a request. */
export const normalizeQuestionWizard = (
  state: QuestionWizardState | undefined,
  questionCount: number,
): QuestionWizardState => {
  const count = Math.max(0, questionCount);
  if (state === undefined) return createQuestionWizard(count);
  return {
    step: Math.min(Math.max(0, state.step), count),
    selections: Array.from(
      { length: count },
      (_, index) => [...(state.selections[index] ?? [])],
    ),
    otherTexts: Array.from(
      { length: count },
      (_, index) => state.otherTexts[index] ?? "",
    ),
  };
};

const withSelection = (
  state: QuestionWizardState,
  index: number,
  selection: readonly string[],
): QuestionWizardState => ({
  ...state,
  selections: state.selections.map((value, candidate) =>
    candidate === index ? [...selection] : value
  ),
});

export const toggleQuestionOption = (
  state: QuestionWizardState,
  questions: readonly Question[],
  optionId: string,
): QuestionWizardState => {
  const normalized = normalizeQuestionWizard(state, questions.length);
  const question = questions[normalized.step];
  if (question === undefined) return normalized;
  if (
    optionId !== OTHER_OPTION_ID
    && !question.options.some((option) => option.id === optionId)
  ) return normalized;

  const selection = normalized.selections[normalized.step] ?? [];
  if (question.allow_multiple === true) {
    const next = selection.includes(optionId)
      ? selection.filter((selected) => selected !== optionId)
      : [...selection, optionId];
    return withSelection(normalized, normalized.step, next);
  }
  return withSelection(
    normalized,
    normalized.step,
    selection[0] === optionId ? [] : [optionId],
  );
};

export const editQuestionOther = (
  state: QuestionWizardState,
  questionCount: number,
  text: string,
): QuestionWizardState => {
  const normalized = normalizeQuestionWizard(state, questionCount);
  if (normalized.step >= questionCount) return normalized;
  return {
    ...normalized,
    otherTexts: normalized.otherTexts.map((value, index) =>
      index === normalized.step ? text : value
    ),
  };
};

export const canAdvanceQuestionWizard = (
  state: QuestionWizardState,
  questionCount: number,
): boolean => {
  const normalized = normalizeQuestionWizard(state, questionCount);
  if (normalized.step === questionCount) return true;
  const selection = normalized.selections[normalized.step] ?? [];
  return selection.length > 0
    && (
      !selection.includes(OTHER_OPTION_ID)
      || (normalized.otherTexts[normalized.step] ?? "").trim() !== ""
    );
};

export const advanceQuestionWizard = (
  state: QuestionWizardState,
  questionCount: number,
): QuestionWizardState => {
  const normalized = normalizeQuestionWizard(state, questionCount);
  if (!canAdvanceQuestionWizard(normalized, questionCount)) return normalized;
  return { ...normalized, step: Math.min(questionCount, normalized.step + 1) };
};

export const retreatQuestionWizard = (
  state: QuestionWizardState,
  questionCount: number,
): QuestionWizardState => {
  const normalized = normalizeQuestionWizard(state, questionCount);
  return { ...normalized, step: Math.max(0, normalized.step - 1) };
};

export const questionWizardAnswers = (
  questions: readonly Question[],
  state: QuestionWizardState,
): QuestionAnswer[] => {
  const normalized = normalizeQuestionWizard(state, questions.length);
  return questions.map((question, index) => {
    const selection = normalized.selections[index] ?? [];
    const answer: QuestionAnswer = {
      question_id: question.id,
      selected_option_ids: selection.filter((id) => id !== OTHER_OPTION_ID),
    };
    if (selection.includes(OTHER_OPTION_ID)) {
      answer.other_text = normalized.otherTexts[index] ?? "";
    }
    return answer;
  });
};

const optionLabel = (question: Question, id: string): string =>
  question.options.find((option) => option.id === id)?.label ?? id;

export const pendingQuestionSummary = (
  questions: readonly Question[],
  state: QuestionWizardState,
): QuestionAnswerSummary[] => {
  const normalized = normalizeQuestionWizard(state, questions.length);
  return questions.map((question, index) => {
    const parts = (normalized.selections[index] ?? []).map((id) => {
      if (id !== OTHER_OPTION_ID) return optionLabel(question, id);
      const text = (normalized.otherTexts[index] ?? "").trim();
      return text === "" ? "Other" : `Other: ${text}`;
    });
    return { prompt: question.prompt, answer: parts.join(", ") || "—" };
  });
};

export const resolvedQuestionSummary = (
  questions: readonly Question[],
  answers: readonly QuestionAnswer[],
): QuestionAnswerSummary[] => questions.map((question) => {
  const answer = answers.find((candidate) => candidate.question_id === question.id);
  const parts = (answer?.selected_option_ids ?? []).map((id) => optionLabel(question, id));
  if (answer?.other_text !== undefined && answer.other_text !== null) {
    const text = answer.other_text.trim();
    parts.push(text === "" ? "Other" : `Other: ${text}`);
  }
  return { prompt: question.prompt, answer: parts.join(", ") || "—" };
});
