# Fonts

## JetBrains Mono italics

The terminal draws JetBrains Mono, which `brrritscold-core-gpui` bundles as `font-family.mono`
in weights 400 and 700, upright only. Programs print italic (SGR 3), so the terminal adds the
two italic faces, from the same release core-gpui vendors its faces from:
[JetBrains Mono 2.304](https://github.com/JetBrains/JetBrainsMono/releases/tag/v2.304),
`fonts/ttf/` in `JetBrainsMono-2.304.zip`, unmodified. Licensed under the SIL Open Font
License 1.1 ([`JetBrainsMono-OFL.txt`](JetBrainsMono-OFL.txt), the release's `OFL.txt`).

They stay here, not in core-gpui. core-gpui follows core-react, and core-react loads only
the upright `@fontsource/jetbrains-mono` faces: the design system sets no italic text. Move
them only if core-react starts shipping italics; then core-gpui follows and these go.

| File | SHA-256 |
|---|---|
| `JetBrainsMono-2.304.zip` | `6f6376c6ed2960ea8a963cd7387ec9d76e3f629125bc33d1fdcd7eb7012f7bbf` |
| `JetBrainsMono-Italic.ttf` | `9d0a1f7a708e6af183f1193b7e81d40da294f5c67682c085d8401c60aac8ded4` |
| `JetBrainsMono-BoldItalic.ttf` | `4039d5ce0ed225bf9c8b2c8c6436290ae2f356b7e90d70fa666227238324aa3b` |

## Symbol fallbacks

JetBrains Mono has no glyph for several characters agents print (`⎿`, `✻`, `◐`, `✳`, `✔`, the
braille spinners), so the terminal falls back to these two families, bundled so every machine
draws the same glyphs:

| File | Family | Covers, among others |
|---|---|---|
| `NotoSansSymbols2-Regular.ttf` | Noto Sans Symbols 2 | `✻ ◐ ✳ ✔ ✘ ⏺`, braille |
| `NotoSansSymbols-Regular.ttf` | Noto Sans Symbols | `⎿` |

Both are unmodified from `unhinted/ttf/` in the official
[notofonts/symbols](https://github.com/notofonts/symbols) releases, licensed under the SIL Open
Font License 1.1 ([`NotoSans-OFL.txt`](NotoSans-OFL.txt), both releases' identical `OFL.txt`).

| File | Release | SHA-256 |
|---|---|---|
| `NotoSansSymbols2-v2.008.zip` | `NotoSansSymbols2-v2.008` | `346c930bbe8eb946701a05c54e9c11a2094dee1d93c387bf1771c0a3e335688f` |
| `NotoSansSymbols2-Regular.ttf` | | `c4a0a80f0041ce4be81e2478faad22776d23edb98ae3f0d19bd37044820ecf9d` |
| `NotoSansSymbols-v2.003.zip` | `NotoSansSymbols-v2.003` | `0c113cdcf6c31d050b80dac39fba2d804a6985281012e76e9220c0a00da007f3` |
| `NotoSansSymbols-Regular.ttf` | | `6eea9cb4cd39269ea9f95ba5c2735f80ae74049dfc9e1a7c932a5cfc8f0c3030` |
