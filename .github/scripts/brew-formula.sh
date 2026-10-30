#!/usr/bin/env bash
# Makes dist's Homebrew formula install the man pages and shell completions where Homebrew
# links them, instead of leaving them in share/dfdu with the other leftover files, and gives
# it the `test do` block `brew audit` asks for. Fails when the formula does not have the lines
# this expects (a newer dist template), so the change is noticed instead of silently lost.
# Usage: brew-formula.sh <formula.rb>
set -euo pipefail

formula=$1
leftover='    leftover_contents = Dir["*"] - doc_files'
if ! grep -qF 'man1.install' "$formula"; then
    if ! grep -qxF "$leftover" "$formula"; then
        echo "::error::$formula has no line '$leftover'; update .github/scripts/brew-formula.sh" >&2
        exit 1
    fi
    LEFTOVER=$leftover perl -0pi -e '
        my $old = quotemeta $ENV{LEFTOVER};
        my $new = join "\n",
            q{    man1.install Dir["man/*.1"]},
            q{    bash_completion.install "completions/dfdu.bash" => "dfdu"},
            q{    zsh_completion.install "completions/_dfdu"},
            q{    fish_completion.install "completions/dfdu.fish"},
            q{    leftover_contents = Dir["*"] - doc_files - ["man", "completions"]};
        s/^$old$/$new/m or die "no leftover line\n";
    ' "$formula"
fi
if ! grep -qxF '  test do' "$formula"; then
    # The formula's last lines close `install` and the class.
    perl -0pi -e '
        my $test = join "\n",
            q{  end},
            q{},
            q{  test do},
            q{    assert_match version.to_s, shell_output("#{bin}/dfdu --version")},
            q{  end},
            q{end},
            q{};
        s/^  end\nend\n\z/$test/m or die "no end of install and the class\n";
    ' "$formula" || {
        echo "::error::$formula does not end with install's and the class's end; update .github/scripts/brew-formula.sh" >&2
        exit 1
    }
fi
