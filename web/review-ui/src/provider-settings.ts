import type { KnownProvider, Provider } from "./types";

export function providerNeedsCursorSdkMigration(
  provider: Pick<Provider, "kind">,
): boolean {
  return provider.kind === "cursor-cli";
}

export function cursorSdkPreset(
  providers: readonly KnownProvider[],
): KnownProvider | undefined {
  return providers.find((provider) => provider.kind === "cursor-sdk");
}

export function savedProviderMessage(
  displayName: string,
  provider: Pick<Provider, "has_credentials">,
): string {
  return provider.has_credentials
    ? `Saved ${displayName}`
    : `Saved ${displayName}, but provider credentials are still required`;
}
