interface DownloadSample {
  bytes: number;
  at: number;
  rate: number;
}

/** Native-parity download-rate estimator. Consecutive samples are smoothed
 * so one uneven progress poll does not make the transfer label jump. */
export class DownloadRateTracker {
  readonly #now: () => number;
  readonly #samples = new Map<string, DownloadSample>();

  constructor(now: () => number = () => performance.now()) {
    this.#now = now;
  }

  update(key: string, bytes: number): number | undefined {
    const normalizedBytes = Number.isFinite(bytes) ? Math.max(0, bytes) : 0;
    const now = this.#now();
    const sample = this.#samples.get(key);
    if (sample === undefined || normalizedBytes < sample.bytes || now < sample.at) {
      this.#samples.set(key, { bytes: normalizedBytes, at: now, rate: 0 });
      return undefined;
    }
    const elapsedSeconds = (now - sample.at) / 1_000;
    // Independent UI updates can observe the same progress sample. Ignore
    // those instead of treating them as a stalled transfer.
    if (elapsedSeconds < 0.3) return sample.rate > 0 ? sample.rate : undefined;
    const instantaneous = (normalizedBytes - sample.bytes) / elapsedSeconds;
    sample.rate = sample.rate > 0
      ? 0.6 * sample.rate + 0.4 * instantaneous
      : instantaneous;
    sample.bytes = normalizedBytes;
    sample.at = now;
    return sample.rate > 0 ? sample.rate : undefined;
  }

  delete(key: string): void {
    this.#samples.delete(key);
  }

  retain(keys: ReadonlySet<string>): void {
    for (const key of this.#samples.keys()) {
      if (!keys.has(key)) this.#samples.delete(key);
    }
  }

  clear(): void {
    this.#samples.clear();
  }
}

export const formatDownloadRate = (bytesPerSecond: number | undefined): string => {
  if (bytesPerSecond === undefined || !Number.isFinite(bytesPerSecond) || bytesPerSecond <= 0) {
    return "";
  }
  if (bytesPerSecond >= 1_000_000) return `${(bytesPerSecond / 1_000_000).toFixed(1)} MB/s`;
  if (bytesPerSecond >= 1_000) return `${(bytesPerSecond / 1_000).toFixed(0)} kB/s`;
  return `${bytesPerSecond.toFixed(0)} B/s`;
};
