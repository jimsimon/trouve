import type { KnownProvider, Provider } from "./types";

export function providerSetupGroups(
  providers: readonly KnownProvider[],
): { subscriptionProviders: KnownProvider[]; apiProviders: KnownProvider[] } {
  const subscriptionProviders = providers.filter(
    (provider) => provider.category === "subscription"
      || provider.auth === "cli"
      || provider.auth === "oauth",
  );
  const subscriptionIds = new Set(subscriptionProviders.map((provider) => provider.id));
  return {
    subscriptionProviders,
    // Unlike the desktop settings screen, review-ui has no separate Local
    // section. Keep local presets in this custom/API form so they remain
    // selectable rather than disappearing from settings.
    apiProviders: providers.filter((provider) => !subscriptionIds.has(provider.id)),
  };
}

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

export function consumeCursorMigrationFocusRequest(
  request: number,
  provider: Pick<KnownProvider, "kind" | "auth"> | undefined,
  input: { focus: () => void } | null,
): number {
  if (
    request <= 0
    || provider?.kind !== "cursor-sdk"
    || provider.auth !== "api-key"
    || !input
  ) {
    return request;
  }
  input.focus();
  return 0;
}

export function savedProviderMessage(
  displayName: string,
  provider: Pick<Provider, "has_credentials">,
): string {
  return provider.has_credentials
    ? `Saved ${displayName}`
    : `Saved ${displayName}, but provider credentials are still required`;
}
