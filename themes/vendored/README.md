# Vendored themes

These JSON files are copied verbatim from
[anomalyco/opencode](https://github.com/anomalyco/opencode), MIT licensed,
under `packages/opencode/src/cli/cmd/tui/context/theme/`.

Each file follows the OpenCode TUI theme schema
(<https://opencode.ai/theme.json>): a `defs` table of named hex colors
plus a `theme` table mapping semantic roles to those defs. Both a `dark`
and a `light` variant live in every file. `themes/loader.py` converts
each into a pair of `textual.theme.Theme` objects.

To add another OpenCode theme, drop its JSON here and restart the app.
