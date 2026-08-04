from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import generate_web_themes


class GenerateWebThemesTests(unittest.TestCase):
    def test_argb_is_reordered_for_css(self) -> None:
        self.assertEqual(generate_web_themes.css_color(0xFF112233), "#112233")
        self.assertEqual(generate_web_themes.css_color(0x80112233), "#11223380")

    def test_current_source_exports_every_palette_and_role(self) -> None:
        themes = generate_web_themes.parse_themes(
            generate_web_themes.SOURCE.read_text(encoding="utf-8")
        )
        self.assertEqual(
            [theme.id for theme in themes],
            [
                "dark",
                "light",
                "high-contrast-dark",
                "colorblind-dark",
                "colorblind-light",
            ],
        )
        roles = set(themes[0].palette)
        self.assertIn("win_bg", roles)
        self.assertIn("diff_add_bg", roles)
        self.assertIn("diff_del_bg", roles)
        self.assertTrue(all(set(theme.palette) == roles for theme in themes))

    def test_generated_css_uses_semantic_names_and_default_root(self) -> None:
        css = generate_web_themes.render_css(
            generate_web_themes.parse_themes(
                generate_web_themes.SOURCE.read_text(encoding="utf-8")
            )
        )
        self.assertIn(':root,\n[data-theme="dark"]', css)
        self.assertIn('[data-theme="high-contrast-dark"]', css)
        self.assertIn("--trouve-win-bg: #141414;", css)
        self.assertIn("--trouve-scrim: #000000aa;", css)


if __name__ == "__main__":
    unittest.main()
