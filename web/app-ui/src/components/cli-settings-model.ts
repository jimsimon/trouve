import type {
  ProtocolCliInfo,
  ProtocolCliInstallStatus,
} from "../services/protocol-client.js";
import { formatDownloadRate } from "../services/download-rate.js";

export const CLI_POLL_INTERVAL_MS = 1_000;
export const MAX_CLI_POLL_ATTEMPTS = 600;

export const idleCliInstallStatus = (): ProtocolCliInstallStatus => ({
  status: "none",
  received_bytes: 0,
  total_bytes: 0,
});

/** Installation source remains authoritative when version probing fails. */
export const cliIsInstalled = (
  cli: Pick<ProtocolCliInfo, "source">,
): boolean => cli.source === "managed" || cli.source === "path";

export const cliSourceLabel = (
  cli: Pick<ProtocolCliInfo, "source">,
): string => {
  switch (cli.source) {
    case "managed": return "Managed by trouve";
    case "path": return "System PATH";
    case "none": return "Not installed";
    default: return "Unknown source";
  }
};

export const cliVersionLabel = (
  cli: Pick<
    ProtocolCliInfo,
    "source" | "installed_version" | "latest_version" | "update_available"
  >,
): string => {
  if (!cliIsInstalled(cli)) return "Not installed";
  const version = cli.installed_version || "Installed";
  const origin = cli.source === "managed" ? "managed by trouve" : "system PATH";
  const update = cli.update_available && cli.latest_version
    ? ` · ${cli.latest_version} available`
    : "";
  return `${version} · ${origin}${update}`;
};

export const cliPrimaryActionLabel = (
  cli: Pick<ProtocolCliInfo, "source" | "update_available">,
): string => {
  if (!cliIsInstalled(cli)) return "Install";
  if (cli.update_available) return "Update";
  return "Reinstall";
};

export const formatCliBytes = (bytes: number | undefined): string => {
  if (bytes === undefined || !Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"] as const;
  const exponent = Math.min(
    units.length - 1,
    Math.floor(Math.log(bytes) / Math.log(1024)),
  );
  const value = bytes / 1024 ** exponent;
  return `${value >= 10 || exponent === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[exponent]}`;
};

export const cliProgressPercent = (
  status: Pick<ProtocolCliInstallStatus, "received_bytes" | "total_bytes">,
): number | undefined => {
  const received = Math.max(0, status.received_bytes ?? 0);
  const total = status.total_bytes ?? 0;
  if (!Number.isFinite(received) || !Number.isFinite(total) || total <= 0) return undefined;
  return Math.max(0, Math.min(100, Math.round((received / total) * 100)));
};

export const cliProgressLabel = (
  status: ProtocolCliInstallStatus,
  bytesPerSecond?: number,
): string => {
  if (status.status !== "pending") return "";
  const version = status.version ? ` ${status.version}` : "";
  const received = Math.max(0, status.received_bytes ?? 0);
  const total = status.total_bytes ?? 0;
  const percent = cliProgressPercent(status);
  if (percent !== undefined) {
    const rate = formatDownloadRate(bytesPerSecond);
    return `Downloading${version} · ${formatCliBytes(received)} of ${formatCliBytes(total)} · ${percent}%${rate === "" ? "" : ` · ${rate}`}`;
  }
  const rate = formatDownloadRate(bytesPerSecond);
  return received > 0
    ? `Downloading${version} · ${formatCliBytes(received)}${rate === "" ? "" : ` · ${rate}`}`
    : `Preparing${version || " download"}…`;
};

export const pendingCliIds = (
  statuses: ReadonlyMap<string, ProtocolCliInstallStatus>,
): readonly string[] => [...statuses.entries()]
  .filter(([, status]) => status.status === "pending")
  .map(([id]) => id);

export const shouldPollCliInstalls = (
  statuses: ReadonlyMap<string, ProtocolCliInstallStatus>,
  completedAttempts: number,
  maxAttempts = MAX_CLI_POLL_ATTEMPTS,
): boolean => completedAttempts < maxAttempts && pendingCliIds(statuses).length > 0;
