# Themes

dfdu has five built-in themes: `dark`, `light`, `ansi` (the terminal's own 16 colours), `mono` (no colours, only bold, dim and reverse) and `high-contrast`. `--theme auto`, the default, asks the terminal for its background colour at start and picks `dark` or `light`, or `ansi` when the terminal does not answer within a second. `NO_COLOR` turns colours off unless `--color` names a level; `--color 16|256|truecolor` forces one. The command palette (`:`) switches themes while dfdu runs.

## Theme files

A theme file is TOML in `dfdu/themes/NAME.toml` in the configuration directory (`~/.config/dfdu/themes/` on Linux and macOS, `%APPDATA%\dfdu\themes\` on Windows), chosen with `--theme NAME`:

```toml
name = "nord"            # shown in the palette; the file name if left out
base = "dark"            # unset tokens come from this built-in theme

[tokens]
fg          = "#d8dee9"
dir         = "#88c0d0"
"cat.cache" = { rgb = "#E69F00", ansi256 = 178, ansi16 = "yellow" }
"cursor.bg" = { rgb = "#3b4252", ansi16 = "reset", modifiers = ["reversed"] }
```

A key is a token name, which sets its foreground (the background for `bg`), or a token name followed by `.fg` or `.bg`. A value is:

- a hex colour, `"#rrggbb"`, converted to the nearest 256- and 16-colour values on terminals with fewer colours;
- a named ANSI colour (`black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `gray`, `darkgray`, the light forms such as `lightblue`, `white`, or `reset` for the terminal's default), used at every level;
- a table with `rgb`, `ansi256` (0-255) and `ansi16` values for each level, and `modifiers`, a list of `bold`, `dim`, `italic`, `underlined`, `slow_blink`, `rapid_blink`, `reversed`, `hidden` and `crossed_out`, which replaces the base theme's modifiers for that token.

Unknown keys, tokens, colours and modifiers are errors that name the file and the key; dfdu does not start with a broken theme.

## Tokens

| Token | Used for |
|---|---|
| `fg`, `fg.dim`, `bg` | Text, secondary text, the background |
| `border`, `border.focus` | Pane borders, the focused pane's border |
| `header`, `title` | The header line, pane titles |
| `cursor` | The selected row (`cursor.bg` for its background) |
| `mark` | Marked entries |
| `dir` | Directory names |
| `key` | Key names in the footer and help |
| `bar.fill`, `bar.empty` | Size bars |
| `size.big` | Sizes of the largest entries |
| `scan.active`, `scan.done` | The scan indicator while scanning and when done |
| `status.error`, `status.warn`, `status.ok` | Errors, warnings (and growth since the last scan), confirmations |
| `match` | Filter matches in names |
| `cat.cache`, `cat.build`, `cat.vm`, `cat.toolchain`, `cat.media`, `cat.download`, `cat.container`, `cat.other` | Category colours in the list, details and treemap |
| `safety.safe`, `safety.review`, `safety.keep` | Safety levels in the cleanup view |

The category colours of the built-in themes are the Okabe-Ito palette, which stays distinguishable with the common forms of colour blindness; every category also has its name or a glyph, so no information is carried by colour alone.
