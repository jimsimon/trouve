export interface PwaInstallChoice {
  readonly outcome: "accepted" | "dismissed";
  readonly platform?: string;
}

/** Chromium's deliberately non-standard install event. Keep it behind this
 * small adapter so application components do not depend on browser-specific
 * event shapes. */
export interface PwaInstallPromptEvent extends Event {
  prompt(): Promise<void>;
  readonly userChoice: Promise<PwaInstallChoice>;
}

export type PwaInstallResult = "accepted" | "dismissed" | "failed";

export const requestPwaInstall = async (
  event: PwaInstallPromptEvent,
): Promise<PwaInstallResult> => {
  try {
    await event.prompt();
    const choice = await event.userChoice;
    return choice.outcome;
  } catch {
    return "failed";
  }
};

export interface StandaloneEnvironment {
  readonly matchMedia?: (query: string) => Pick<MediaQueryList, "matches">;
  readonly navigator?: Navigator & { readonly standalone?: boolean };
}

/** Installed PWAs should not advertise installation again. Safari exposes a
 * legacy navigator flag; other engines use the display-mode media query. */
export const isStandalonePwa = (
  environment: StandaloneEnvironment = globalThis,
): boolean => {
  try {
    if (environment.matchMedia?.("(display-mode: standalone)").matches === true) {
      return true;
    }
  } catch {
    // A hostile or incomplete test/browser environment is simply not known
    // to be standalone.
  }
  return environment.navigator?.standalone === true;
};
