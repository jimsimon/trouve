export interface HistoryMeasurementCorrection {
  readonly id: string;
  readonly previouslyMeasured: boolean;
  readonly delta: number;
}

/** Keep only genuine late history layout changes during native momentum.
 * First measurements replace estimates and must not move the browser. */
export const retainedHistoryScrollDelta = (
  corrections: readonly HistoryMeasurementCorrection[],
): number => corrections.reduce(
  (retained, correction) =>
    correction.previouslyMeasured && correction.id.startsWith("turn:")
      ? retained + correction.delta
      : retained,
  0,
);
