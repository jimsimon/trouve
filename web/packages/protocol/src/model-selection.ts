/** Model-selection helpers shared by every frontend that renders a model
 * picker or resolves a persisted model id against the protocol catalog.
 *
 * Selections persist in three shapes: current catalog ids (`auto/<model>` or
 * `<provider>/<model>`), pre-auto-namespace bare ids, and concrete route
 * pins that only appear inside an automatic entry's `routes`. */

export interface ModelSelectionCatalogEntry {
  readonly id: string;
  readonly routes?: readonly {
    readonly provider_id: string;
    readonly provider_model: string;
  }[];
}

/** Resolve current ids, pre-auto-namespace bare ids, and concrete route pins. */
export const modelForSelection = <T extends ModelSelectionCatalogEntry>(
  models: readonly T[],
  selection: string | null | undefined,
): T | undefined => {
  if (!selection) return undefined;
  const exact = models.find((model) => model.id === selection);
  if (exact !== undefined) return exact;
  if (!selection.includes("/")) {
    const automatic = models.find((model) => model.id === `auto/${selection}`);
    if (automatic !== undefined) return automatic;
  }
  return models.find((model) => model.routes?.some(
    (route) => `${route.provider_id}/${route.provider_model}` === selection,
  ));
};

/** Canonical picker value for a persisted selection. */
export const modelSelectionValue = (
  models: readonly ModelSelectionCatalogEntry[],
  selection: string | null | undefined,
): string => {
  if (!selection) return "";
  if (
    !selection.includes("/")
    && models.some((model) => model.id === `auto/${selection}`)
  ) {
    return `auto/${selection}`;
  }
  return selection;
};
