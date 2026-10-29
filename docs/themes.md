# Themes

dfdu ships with five themes and can load your own from a small TOML file.

## Built-in themes

| Theme           | Description                                                                                                                                      |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| `dark`          | For dark backgrounds (black to `#2e3440`). Full colour, with 16-colour fallbacks.                                                                |
| `light`         | For light backgrounds (white to `#fdf6e3`). Full colour, with 16-colour fallbacks.                                                               |
| `ansi`          | Only your terminal's own 16 colours, so it follows your terminal's colour scheme. Never uses black or white text, so it reads on any background. |
| `high-contrast` | `ansi` in bold, without dimmed text.                                                                                                             |
| `mono`          | No colours at all, only bold, dim and reverse.                                                                                                   |

Choose one with `--theme`:

```sh
dfdu --theme light ~
```

The default, `--theme auto`, asks your terminal for its background colour when dfdu starts and picks `dark` or `light` to match. If the terminal does not answer within a second, it uses `ansi`.

You can also switch themes while dfdu is running from the command palette (`:`).

### Colour support

dfdu detects how many colours your terminal supports. To override it, pass `--color 16`, `--color 256` or `--color truecolor`.

If `NO_COLOR` is set, dfdu uses no colours (the `mono` theme) unless you pass `--color`.

### Bars

Size bars use eighth-block characters (`█▌`). If your terminal or font has no block characters, pass `--bars ascii`: bars are drawn with `#`, and Map tiles are framed with `=`, `|` and `+` and shaded with `#`, `%`, `+` and `.` (by depth; `.` also for the tile of entries too small to show) in place of `█▓▒░`, and cleanup candidates are hatched with `/` in place of `╱`. A theme file can set the same with `bars = "ascii"`; `--bars` overrides it. Every built-in theme, `mono` included, uses `unicode`.

## Writing a theme

A theme is a TOML file in the `dfdu/themes` folder of your configuration directory:

| Platform     | Folder                                                        |
| ------------ | ------------------------------------------------------------- |
| macOS, Linux | `~/.config/dfdu/themes/` (or `$XDG_CONFIG_HOME/dfdu/themes/`) |
| Windows      | `%APPDATA%\dfdu\themes\`                                      |

The file name is the theme's name. Save a theme as `~/.config/dfdu/themes/nord.toml` and use it with:

```sh
dfdu --theme nord
```

A theme starts from one of the built-in themes and changes only the colours you list. Here is a complete example:

```toml
name = "nord"            # shown in the command palette; defaults to the file name
base = "dark"            # built-in theme to start from; defaults to dark
bars = "unicode"         # or "ascii"; defaults to unicode

[tokens]
fg             = "#d8dee9"
"fg.dim"       = "#4c566a"
dir            = "#88c0d0"
"border.focus" = "#88c0d0"
"status.error" = "#bf616a"
"status.warn"  = "#ebcb8b"
"status.ok"    = "#a3be8c"
"cursor.bg"    = "#3b4252"
"cat.cache"    = { rgb = "#e69f00", ansi256 = 178, ansi16 = "yellow" }
title          = { rgb = "#eceff4", modifiers = ["bold"] }
```

Keys that contain a dot need quotes in TOML (`"fg.dim"`, not `fg.dim`).

If anything in the file is wrong (an unknown key, token, colour or modifier), dfdu does not start and prints the file and key at fault, so a typo never goes unnoticed.

### Keys

Each key under `[tokens]` is a [token](#tokens) name:

- `dir = ...` sets the token's foreground (text) colour. For `bg` it sets the background.
- `"dir.fg" = ...` and `"dir.bg" = ...` set the foreground or background explicitly.

### Values

A value can be any of:

**A hex colour**, `"#rrggbb"`. On terminals with 256 or 16 colours, dfdu uses the nearest colour they have.

```toml
dir = "#88c0d0"
```

**A named ANSI colour**, used as is on every terminal: `black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `gray`, `darkgray`, `white`, the light forms such as `lightblue` and `lightred`, or `reset` for the terminal's default colour.

```toml
dir = "cyan"
```

**A table**, to choose the colour for each level of colour support yourself and to set text styles. Every field is optional.

```toml
"cat.cache" = { rgb = "#e69f00", ansi256 = 178, ansi16 = "yellow", modifiers = ["bold"] }
```

| Field       | Meaning                                                                                                                                                                                   |
| ----------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `rgb`       | Hex colour for truecolor terminals                                                                                                                                                        |
| `ansi256`   | Colour number (0-255) for 256-colour terminals                                                                                                                                            |
| `ansi16`    | Named colour for 16-colour terminals                                                                                                                                                      |
| `modifiers` | Text styles: `bold`, `dim`, `italic`, `underlined`, `reversed`, `crossed_out`, `slow_blink`, `rapid_blink`, `hidden`. Replaces the base theme's styles for this token; `[]` removes them. |

Levels you leave out are worked out from the ones you give. A table with only `modifiers` keeps the base theme's colour:

```toml
title = { modifiers = ["bold", "underlined"] }
```

## Tokens

| Token                                         | Used for                                          |
| --------------------------------------------- | ------------------------------------------------- |
| `fg`                                          | Normal text                                       |
| `fg.dim`                                      | Secondary text                                    |
| `bg`                                          | Background                                        |
| `border`                                      | Pane borders                                      |
| `border.focus`                                | Border of the focused pane                        |
| `header`                                      | The header line                                   |
| `title`                                       | Pane titles                                       |
| `cursor`                                      | The selected row (`cursor.bg` for its background) |
| `mark`                                        | Marked entries                                    |
| `dir`                                         | Directory names                                   |
| `key`                                         | Key names in the footer and help                  |
| `bar.fill`, `bar.empty`                       | Size bars                                         |
| `size.big`                                    | Sizes of the largest entries                      |
| `scan.active`                                 | Scan indicator while scanning                     |
| `scan.done`                                   | Scan indicator when done                          |
| `status.error`                                | Errors                                            |
| `status.warn`                                 | Warnings, and growth since the last scan          |
| `status.ok`                                   | Confirmations                                     |
| `match`                                       | Filter matches in names                           |
| `safety.safe`, `safety.review`, `safety.keep` | Safety levels in the cleanup view                 |

### Category colours

Entries matched by a [cleanup rule](rules.md) are tagged with their category. Categories share eight colours:

| Token           | Categories                                                                                     |
| --------------- | ---------------------------------------------------------------------------------------------- |
| `cat.cache`     | Package manager caches, IDE caches, OS and user caches, browser caches, chat apps              |
| `cat.build`     | Project build output, project dependencies                                                     |
| `cat.vm`        | Android SDK and emulators, mobile device data                                                  |
| `cat.toolchain` | Xcode and simulators, toolchains and runtimes, Nix                                             |
| `cat.media`     | Large video files, media apps, games                                                           |
| `cat.download`  | Installers and disk images, extracted archives, Trash, OS update leftovers                     |
| `cat.container` | Containers and VMs                                                                             |
| `cat.other`     | Everything else: logs, crash reports, large old files, duplicates, empty folders, broken links |

The built-in themes use the [Okabe-Ito palette](https://jfly.uni-koeln.de/color/) for these, which stays distinguishable with the common forms of colour blindness. Every category is also shown by name or glyph, so no information depends on colour alone. If you change these colours, keep them distinct from each other.
