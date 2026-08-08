import "@fortawesome/fontawesome-free/css/solid.css";

import { html, nothing, type TemplateResult } from "lit";

/**
 * The locally bundled subset of Font Awesome Free used by trouve. Keeping the
 * mapping explicit avoids shipping Font Awesome's complete CSS class catalog
 * or resolving icons from a CDN. The glyphs come from the 7.3.1 free solid
 * font included by @fortawesome/fontawesome-free.
 */
export const FONT_AWESOME_CODEPOINTS = Object.freeze({
  "arrow-down": 0xf063,
  "arrow-left": 0xf060,
  "arrow-right": 0xf061,
  "arrow-turn-down": 0xf149,
  "arrow-up": 0xf062,
  "arrow-up-right-from-square": 0xf08e,
  "arrows-rotate": 0xf021,
  ban: 0xf05e,
  brain: 0xf5dc,
  "caret-down": 0xf0d7,
  "caret-right": 0xf0da,
  "caret-up": 0xf0d8,
  check: 0xf00c,
  circle: 0xf111,
  "circle-dot": 0xf192,
  "circle-exclamation": 0xf06a,
  "circle-half-stroke": 0xf042,
  "circle-info": 0xf05a,
  "circle-question": 0xf059,
  code: 0xf121,
  "code-branch": 0xf126,
  "code-compare": 0xe13a,
  "code-merge": 0xf387,
  "code-pull-request": 0xe13c,
  comments: 0xf086,
  copy: 0xf0c5,
  ellipsis: 0xf141,
  eye: 0xf06e,
  file: 0xf15b,
  "file-import": 0xf56f,
  "file-image": 0xf1c5,
  "file-lines": 0xf15c,
  folder: 0xf07b,
  "folder-open": 0xf07c,
  "folder-tree": 0xf802,
  gear: 0xf013,
  "grip-vertical": 0xf58e,
  list: 0xf03a,
  "list-check": 0xf0ae,
  "magnifying-glass": 0xf002,
  message: 0xf27a,
  paperclip: 0xf0c6,
  pause: 0xf04c,
  pen: 0xf304,
  play: 0xf04b,
  plug: 0xf1e6,
  plus: 0x2b,
  "rotate-left": 0xf2ea,
  "rotate-right": 0xf2f9,
  route: 0xf4d7,
  spinner: 0xf110,
  square: 0xf0c8,
  "square-check": 0xf14a,
  stopwatch: 0xf2f2,
  terminal: 0xf120,
  "trash-can": 0xf2ed,
  "triangle-exclamation": 0xf071,
  user: 0xf007,
  "user-plus": 0xf234,
  xmark: 0xf00d,
});

export type FontAwesomeIconName = keyof typeof FONT_AWESOME_CODEPOINTS;

export interface FontAwesomeIconOptions {
  readonly className?: string;
  /** Supply a label only when the icon conveys meaning without adjacent text. */
  readonly label?: string;
  readonly spin?: boolean;
}

const ICON_STYLE = [
  "display:inline-block",
  "flex:none",
  "width:var(--trouve-icon-width,1.25em)",
  "font-family:'Font Awesome 7 Free'",
  "font-style:normal",
  "font-weight:900",
  "font-synthesis:none",
  "line-height:1",
  "text-align:center",
  "text-rendering:auto",
  "-webkit-font-smoothing:antialiased",
  "-moz-osx-font-smoothing:grayscale",
].join(";");

export const fontAwesomeIcon = (
  name: FontAwesomeIconName,
  options: FontAwesomeIconOptions = {},
): TemplateResult => {
  const label = options.label?.trim() ?? "";
  const className = [
    "trouve-icon",
    options.spin === true ? "trouve-icon-spin" : "",
    options.className ?? "",
  ].filter(Boolean).join(" ");
  return html`<span
    class=${className}
    data-font-awesome-icon=${name}
    style=${ICON_STYLE}
    role=${label === "" ? nothing : "img"}
    aria-label=${label === "" ? nothing : label}
    aria-hidden=${label === "" ? "true" : nothing}
  >${Number.isInteger(FONT_AWESOME_CODEPOINTS[name])
    ? String.fromCodePoint(FONT_AWESOME_CODEPOINTS[name])
    : "□"}</span>`;
};
