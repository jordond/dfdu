// The text of /llms.txt (https://llmstxt.org).

const site = 'https://dfdu.app'
const repo = 'https://github.com/jordond/dfdu'

/** @type {import('starlight-llms-txt').StarlightLllmsTextOptions} */
export const llms = {
  projectName: 'dfdu',
  description:
    'dfdu is a disk usage browser for the terminal, written in Rust, for macOS, Linux and Windows. It scans a directory in parallel, lets you browse, sort and delete while the scan runs, shows a treemap, duplicates and the largest files, and recognises caches, build output and other files that are safe to remove. It also has non-interactive commands with JSON output for scripts and CI.',
  details: `Install (any one; all put a \`dfdu\` binary on PATH):

\`\`\`sh
# macOS, Linux (Homebrew)
brew install jordond/tap/dfdu
# macOS, Linux, without a package manager
curl -LsSf ${repo}/releases/latest/download/dfdu-installer.sh | sh
# prebuilt, through cargo
cargo binstall --git ${repo} dfdu
# Nix flake, without installing
nix run github:jordond/dfdu
# from source, Rust 1.95+
cargo install --locked --git ${repo} dfdu
\`\`\`

Windows (PowerShell): \`powershell -ExecutionPolicy Bypass -c "irm ${repo}/releases/latest/download/dfdu-installer.ps1 | iex"\`. Release binaries for every platform are at ${repo}/releases.

Run it:

\`\`\`sh
dfdu ~                                       # browse a directory interactively (q quits, ? lists keys)
dfdu scan ~                                  # print a summary, no TUI
dfdu report ~/dev                            # what the cleanup rules found, with safety levels
dfdu report ~/dev --format json --fail-above 20GiB   # CI check: exit 4 over the limit
dfdu clean ~/dev                             # dry run of the recommended deletes
dfdu clean ~/dev --yes                       # move them to the Trash
dfdu diff ~ --since 7d                       # what grew in the last week
\`\`\`

Notes for assistants:

- \`dfdu clean\` changes nothing without \`--yes\`, and deletes go to the Trash. Suggest the dry run first.
- \`dfdu --help\` lists every option and exit code. JSON output is versioned; the schemas are in ${repo}/tree/main/schema.
- Cleanup rules are TOML files the user can add to; each match has a safety level (safe, regenerable, review, manual, protected).

Guides:

- [Overview](${site}/docs/): features and a comparison with ncdu, gdu, dua and Dust
- [Install](${site}/docs/install/): every install method
- [Browsing](${site}/docs/usage/): keys, views, mounts and the scan cache
- [Command line](${site}/docs/cli/): the commands for scripts and CI
- [Cleanup rules](${site}/docs/rules/): the built-in rules, safety levels and how to write your own
- [Themes](${site}/docs/themes/): built-in themes and custom TOML themes
- [Benchmarks](${site}/docs/benchmarks/): scan time and memory against other tools`,
  optionalLinks: [
    { label: 'Source code', url: repo, description: 'the Rust workspace, MIT licensed' },
    { label: 'Releases', url: `${repo}/releases`, description: 'binaries, man pages and shell completions' },
    { label: 'JSON schemas', url: `${repo}/tree/main/schema`, description: 'for the JSON and NDJSON output' },
    { label: 'Contributing', url: `${repo}/blob/main/CONTRIBUTING.md`, description: 'building and testing dfdu' },
  ],
}
