import { describe, expect, it } from "vitest";

import type { components as ProtocolComponents } from "../generated/protocol.js";
import {
  advanceQuestionWizard,
  canAdvanceQuestionWizard,
  createQuestionWizard,
  editQuestionOther,
  normalizeQuestionWizard,
  OTHER_OPTION_ID,
  pendingQuestionSummary,
  questionWizardAnswers,
  resolvedQuestionSummary,
  retreatQuestionWizard,
  toggleQuestionOption,
} from "./question-wizard.js";

type Question = ProtocolComponents["schemas"]["Question"];

const questions: Question[] = [
  {
    id: "color",
    prompt: "Favorite color?",
    options: [
      { id: "red", label: "Red" },
      { id: "blue", label: "Blue" },
    ],
  },
  {
    id: "fruit",
    prompt: "Fruits you like?",
    options: [
      { id: "apple", label: "Apple" },
      { id: "pear", label: "Pear" },
    ],
    allow_multiple: true,
  },
];

describe("question wizard", () => {
  it("requires an answer, allows radio deselection, and reaches review", () => {
    let state = createQuestionWizard(questions.length);
    expect(canAdvanceQuestionWizard(state, questions.length)).toBe(false);
    expect(advanceQuestionWizard(state, questions.length).step).toBe(0);

    state = toggleQuestionOption(state, questions, "red");
    expect(state.selections[0]).toEqual(["red"]);
    expect(canAdvanceQuestionWizard(state, questions.length)).toBe(true);
    state = toggleQuestionOption(state, questions, "red");
    expect(state.selections[0]).toEqual([]);

    state = toggleQuestionOption(state, questions, "blue");
    state = advanceQuestionWizard(state, questions.length);
    expect(state.step).toBe(1);
    state = toggleQuestionOption(state, questions, "apple");
    state = advanceQuestionWizard(state, questions.length);
    expect(state.step).toBe(2);
    expect(retreatQuestionWizard(state, questions.length).step).toBe(1);
  });

  it("toggles multiple choices and requires nonblank Other text", () => {
    let state = createQuestionWizard(questions.length);
    state = toggleQuestionOption(state, questions, "red");
    state = advanceQuestionWizard(state, questions.length);
    state = toggleQuestionOption(state, questions, "apple");
    state = toggleQuestionOption(state, questions, OTHER_OPTION_ID);
    expect(state.selections[1]).toEqual(["apple", OTHER_OPTION_ID]);
    expect(canAdvanceQuestionWizard(state, questions.length)).toBe(false);
    state = editQuestionOther(state, questions.length, "   ");
    expect(canAdvanceQuestionWizard(state, questions.length)).toBe(false);
    state = editQuestionOther(state, questions.length, "mango");
    expect(canAdvanceQuestionWizard(state, questions.length)).toBe(true);
    state = toggleQuestionOption(state, questions, "apple");
    expect(state.selections[1]).toEqual([OTHER_OPTION_ID]);
  });

  it("builds the same review and submission representation as Slint", () => {
    let state = createQuestionWizard(questions.length);
    state = toggleQuestionOption(state, questions, "blue");
    state = advanceQuestionWizard(state, questions.length);
    state = toggleQuestionOption(state, questions, "apple");
    state = toggleQuestionOption(state, questions, OTHER_OPTION_ID);
    state = editQuestionOther(state, questions.length, " mango ");

    expect(pendingQuestionSummary(questions, state)).toEqual([
      { prompt: "Favorite color?", answer: "Blue" },
      { prompt: "Fruits you like?", answer: "Apple, Other: mango" },
    ]);
    expect(questionWizardAnswers(questions, state)).toEqual([
      { question_id: "color", selected_option_ids: ["blue"] },
      {
        question_id: "fruit",
        selected_option_ids: ["apple"],
        other_text: " mango ",
      },
    ]);
  });

  it("renders compact resolved summaries and normalizes stale state", () => {
    expect(resolvedQuestionSummary(questions, [
      { question_id: "color", selected_option_ids: ["red"] },
    ])).toEqual([
      { prompt: "Favorite color?", answer: "Red" },
      { prompt: "Fruits you like?", answer: "—" },
    ]);
    expect(normalizeQuestionWizard({
      step: 9,
      selections: [["red"]],
      otherTexts: [],
    }, 2)).toEqual({
      step: 2,
      selections: [["red"], []],
      otherTexts: ["", ""],
    });
  });
});
