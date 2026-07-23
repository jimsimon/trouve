export interface CliInfo {
  id: string;
  display_name: string;
  kinds: string[];
  installed_version?: string;
  source: string;
  path?: string;
  latest_version?: string;
  update_available: boolean;
}

export interface CliInstallStatus {
  status: "none" | "pending" | "success" | "failed";
  version?: string;
  error?: string;
  received_bytes: number;
  total_bytes: number;
}

export const idleCliInstallStatus = (): CliInstallStatus => ({
  status: "none",
  received_bytes: 0,
  total_bytes: 0,
});

/** The source is authoritative even when a vendor's version command failed. */
export function cliIsInstalled(cli: CliInfo): boolean {
  return cli.source === "managed" || cli.source === "path";
}

export function cliVersionLabel(cli: CliInfo): string {
  if (!cliIsInstalled(cli)) return "Not installed";
  const version = cli.installed_version || "Installed";
  const origin = cli.source === "managed" ? "managed by trouve" : "system PATH";
  const update = cli.update_available && cli.latest_version
    ? ` · ${cli.latest_version} available`
    : "";
  return `${version} · ${origin}${update}`;
}

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  const unit = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  const value = bytes / (1024 ** unit);
  return `${value >= 10 || unit === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`;
}

export function cliProgressLabel(status: CliInstallStatus): string {
  if (status.status !== "pending") return "";
  const version = status.version ? ` ${status.version}` : "";
  if (status.total_bytes > 0) {
    const percent = Math.min(100, Math.round((status.received_bytes / status.total_bytes) * 100));
    return `Downloading${version} · ${formatBytes(status.received_bytes)} of ${formatBytes(status.total_bytes)} · ${percent}%`;
  }
  return status.received_bytes > 0
    ? `Downloading${version} · ${formatBytes(status.received_bytes)}`
    : `Preparing${version || " download"}…`;
}
