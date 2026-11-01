#!/bin/sh
# Installs dfdu, the disk usage browser for the terminal, on macOS and Linux.
#
#   curl -fsSL https://dfdu.app/install.sh | sh
#   curl -fsSL https://dfdu.app/install.sh | sh -s -- --version 0.1.0 --bin-dir /usr/local/bin
#
# Downloads the release archive for this machine from GitHub, checks it against its
# published SHA-256, and puts dfdu in ~/.local/bin, the man pages in ~/.local/share/man and
# the bash, zsh and fish completions in their per-user directories. Run it again to update.
# Nothing outside those directories is changed: no shell profile is edited.
#
# Windows: use the PowerShell installer instead (https://dfdu.app/docs/install/).

# The work is done by main, called on the last line, so a download cut short runs nothing.

set -eu

REPO="jordond/dfdu"

usage() {
	cat <<EOF
Install dfdu from its GitHub releases.

Usage: install.sh [options]
       curl -fsSL https://dfdu.app/install.sh | sh -s -- [options]

Options:
  -b, --bin-dir DIR   Where to put the dfdu binary [default: ~/.local/bin]
                      (env: DFDU_INSTALL_DIR)
  -v, --version VER   Install this version, e.g. 0.1.0 [default: the latest release]
                      (env: DFDU_VERSION)
      --no-extras     Install only the binary, without man pages and completions
  -h, --help          Show this help

If DIR is not writable, the binary is installed with sudo and the extras are skipped.
DFDU_NO_VERIFY=1 installs without a SHA-256 tool (sha256sum, shasum or openssl).
EOF
}

# Colours only on a terminal, and never with NO_COLOR set (https://no-color.org).
if [ -t 2 ] && [ -z "${NO_COLOR-}" ] && [ "${TERM-}" != "dumb" ]; then
	BOLD="$(printf '\033[1m')" DIM="$(printf '\033[2m')" RED="$(printf '\033[31m')"
	GREEN="$(printf '\033[32m')" YELLOW="$(printf '\033[33m')" RESET="$(printf '\033[0m')"
else
	BOLD="" DIM="" RED="" GREEN="" YELLOW="" RESET=""
fi

info() { printf '%s\n' "${DIM}>${RESET} $*" >&2; }
warn() { printf '%s\n' "${YELLOW}! $*${RESET}" >&2; }
ok() { printf '%s\n' "${GREEN}✓${RESET} $*" >&2; }
die() {
	printf '%s\n' "${RED}x $*${RESET}" >&2
	exit 1
}

has() { command -v "$1" >/dev/null 2>&1; }

# A path under $HOME as the reader would type it: ~/... for messages, "$HOME/..." for lines to paste.
home() { h="${HOME:-}" && printf '%s' "${h%/}"; }
under_home() { h="$(home)" && [ -n "$h" ] && case "$1" in "$h" | "$h"/*) true ;; *) false ;; esac }
tilde() { if under_home "$1"; then printf '%s\n' "~${1#"$h"}"; else printf '%s\n' "$1"; fi; }
home_var() { if under_home "$1"; then printf '%s\n' "\$HOME${1#"$h"}"; else printf '%s\n' "$1"; fi; }

download() { # url dest
	if has curl; then
		curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$2" "$1"
	elif has wget; then
		# BusyBox wget has no --https-only; the URLs are all https anyway.
		if wget --help 2>&1 | grep -q -- --https-only; then
			wget --https-only -q -O "$2" "$1"
		else
			wget -q -O "$2" "$1"
		fi
	else
		die "curl or wget is needed to download dfdu"
	fi
}

# The file's SHA-256 in lowercase hex; fails when no tool is found or the tool fails.
sha256() {
	if has sha256sum; then
		out="$(sha256sum "$1")" || return 1
	elif has shasum; then
		out="$(shasum -a 256 "$1")" || return 1
	elif has openssl; then
		out="$(openssl dgst -sha256 -r "$1")" || return 1
	else
		return 2
	fi
	printf '%s\n' "${out%% *}" | tr 'A-F' 'a-f'
}

is_sha256() { [ ${#1} -eq 64 ] && case "$1" in *[!0-9a-f]*) false ;; esac }

detect_target() {
	os="$(uname -s)"
	arch="$(uname -m)"

	case "$arch" in
	x86_64 | amd64) arch="x86_64" ;;
	arm64 | aarch64) arch="aarch64" ;;
	*) die "no dfdu build for the $arch architecture; build it from source: cargo install --locked --git https://github.com/$REPO dfdu" ;;
	esac

	case "$os" in
	Darwin)
		# A shell under Rosetta reports x86_64 on Apple silicon; take the native build.
		if [ "$arch" = "x86_64" ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = "1" ]; then
			arch="aarch64"
		fi
		echo "$arch-apple-darwin"
		;;
	Linux)
		# The Linux builds are static (musl), so they run on glibc and musl systems alike.
		echo "$arch-unknown-linux-musl"
		;;
	MINGW* | MSYS* | CYGWIN* | Windows_NT)
		die "on Windows, install with PowerShell: powershell -ExecutionPolicy Bypass -c \"irm https://github.com/$REPO/releases/latest/download/dfdu-installer.ps1 | iex\""
		;;
	*) die "no dfdu build for $os; build it from source: cargo install --locked --git https://github.com/$REPO dfdu" ;;
	esac
}

# The directory is writable, or can be created, by this user.
writable() {
	dir="$1"
	while [ ! -d "$dir" ]; do dir="$(dirname "$dir")"; done
	[ -w "$dir" ]
}

# Copies one file, warning instead of failing: the binary is already installed.
extra() { # from to
	mkdir -p "$(dirname "$2")" 2>/dev/null && cp "$1" "$2" 2>/dev/null && return 0
	warn "could not install $(tilde "$2")"
	return 1
}

# The man pages and completions, in the per-user directories. Never fails.
install_extras() { # unpacked dir
	if [ -z "${HOME:-}" ] && { [ -z "${XDG_DATA_HOME:-}" ] || [ -z "${XDG_CONFIG_HOME:-}" ]; }; then
		warn "HOME is not set; man pages and completions skipped"
		return 0
	fi
	data="${XDG_DATA_HOME:-$HOME/.local/share}"
	config="${XDG_CONFIG_HOME:-$HOME/.config}"

	man=0
	for page in "$1"/man/*.1; do
		[ -f "$page" ] || continue
		extra "$page" "$data/man/man1/$(basename "$page")" && man=1
	done
	shells=""
	[ -f "$1/completions/dfdu.bash" ] && extra "$1/completions/dfdu.bash" "$data/bash-completion/completions/dfdu" && shells="$shells bash"
	[ -f "$1/completions/_dfdu" ] && extra "$1/completions/_dfdu" "$data/zsh/site-functions/_dfdu" && shells="$shells zsh"
	[ -f "$1/completions/dfdu.fish" ] && extra "$1/completions/dfdu.fish" "$config/fish/completions/dfdu.fish" && shells="$shells fish"

	[ "$man" = 1 ] && info "man pages in ${DIM}$(tilde "$data/man")${RESET}"
	[ -n "$shells" ] && info "completions for$shells"
	case "$shells" in
	*zsh*)
		[ "$(basename "${SHELL-}")" = zsh ] &&
			info "zsh: add ${BOLD}fpath+=($(home_var "$data/zsh/site-functions"))${RESET} before compinit in ~/.zshrc for completions"
		;;
	esac
	return 0
}

# The ~ is shown to the reader, not expanded.
# shellcheck disable=SC2088
path_hint() { # bin dir
	dir="$(home_var "$1")"
	case "$(basename "${SHELL-}")" in
	fish) line="fish_add_path $dir" file="~/.config/fish/config.fish" ;;
	zsh) line="export PATH=\"$dir:\$PATH\"" file="~/.zshrc" ;;
	bash)
		line="export PATH=\"$dir:\$PATH\""
		# macOS terminals start login shells, which read ~/.bash_profile and not ~/.bashrc.
		if [ "$(uname -s)" = Darwin ]; then file="~/.bash_profile"; else file="~/.bashrc"; fi
		;;
	*) line="export PATH=\"$dir:\$PATH\"" file="your shell's profile" ;;
	esac
	warn "$(tilde "$1") is not on your PATH. Add this line to $file, then open a new terminal:"
	printf '\n    %s\n\n' "$line" >&2
}

main() {
	bin_dir="${DFDU_INSTALL_DIR:-}"
	version="${DFDU_VERSION:-}"
	extras=1

	while [ $# -gt 0 ]; do
		case "$1" in
		-b | --bin-dir)
			[ $# -ge 2 ] || die "$1 needs a directory"
			bin_dir="$2"
			[ -n "$bin_dir" ] || die "$1 needs a directory"
			shift 2
			;;
		--bin-dir=*)
			bin_dir="${1#*=}"
			[ -n "$bin_dir" ] || die "--bin-dir needs a directory"
			shift
			;;
		-v | --version)
			[ $# -ge 2 ] || die "$1 needs a version"
			version="$2"
			shift 2
			;;
		--version=*)
			version="${1#*=}"
			shift
			;;
		--no-extras)
			extras=0
			shift
			;;
		-h | --help)
			usage
			exit 0
			;;
		*)
			usage >&2
			die "unknown option: $1"
			;;
		esac
	done

	if [ -z "$bin_dir" ]; then
		[ -n "${HOME:-}" ] || die "HOME is not set; pass --bin-dir DIR"
		bin_dir="$(home)/.local/bin"
	fi
	case "$bin_dir" in /*) ;; *) bin_dir="$PWD/$bin_dir" ;; esac
	while [ "${bin_dir%/}" != "$bin_dir" ] && [ "$bin_dir" != / ]; do bin_dir="${bin_dir%/}"; done

	target="$(detect_target)"
	archive="dfdu-$target.tar.xz"
	if [ -n "$version" ]; then
		version="${version#v}"
		base="https://github.com/$REPO/releases/download/v$version"
	else
		base="https://github.com/$REPO/releases/latest/download"
	fi

	tmp="$(mktemp -d 2>/dev/null || mktemp -d -t dfdu)"
	trap 'rm -rf "$tmp"' EXIT
	trap 'exit 130' INT
	trap 'exit 143' TERM

	info "downloading ${BOLD}$archive${RESET} ${DIM}(${version:-latest})${RESET}"
	download "$base/$archive" "$tmp/$archive" ||
		die "could not download $base/$archive${version:+; is $version a dfdu release? See https://github.com/$REPO/releases}"
	download "$base/$archive.sha256" "$tmp/$archive.sha256" ||
		die "could not download the checksum $base/$archive.sha256"

	expected="$(cut -d ' ' -f 1 <"$tmp/$archive.sha256" | tr 'A-F' 'a-f')"
	is_sha256 "$expected" || die "$archive.sha256 holds no SHA-256"
	rc=0
	actual="$(sha256 "$tmp/$archive")" || rc=$?
	if [ "$rc" -ne 0 ] || ! is_sha256 "$actual"; then
		if [ "${DFDU_NO_VERIFY:-}" = 1 ]; then
			warn "the download is not verified (DFDU_NO_VERIFY=1)"
		elif [ "$rc" -eq 2 ]; then
			die "no sha256sum, shasum or openssl to verify the download; install one, or set DFDU_NO_VERIFY=1 to skip"
		else
			die "could not compute the SHA-256 of $archive"
		fi
	elif [ "$actual" != "$expected" ]; then
		die "checksum mismatch for $archive: expected $expected, got $actual"
	else
		info "checksum verified"
	fi

	tar -xJf "$tmp/$archive" -C "$tmp" 2>/dev/null ||
		die "could not unpack $archive; tar needs xz support (install xz or xz-utils)"
	unpacked="$tmp/dfdu-$target"
	[ -f "$unpacked/dfdu" ] || die "no dfdu binary in $archive"

	if writable "$bin_dir"; then
		mkdir -p "$bin_dir"
		# Copy beside the old binary, then rename over it, so a running dfdu is not overwritten in place.
		cp "$unpacked/dfdu" "$bin_dir/.dfdu.new"
		chmod 755 "$bin_dir/.dfdu.new"
		mv -f "$bin_dir/.dfdu.new" "$bin_dir/dfdu"
	else
		# Under HOME, a directory this user cannot write is a permissions problem; sudo would hide it.
		if under_home "$bin_dir"; then
			die "$(tilde "$bin_dir") is not writable by $(id -un); fix its permissions or pick another directory with --bin-dir"
		fi
		has sudo || die "$bin_dir is not writable and sudo is not installed; pick another directory with --bin-dir"
		warn "$bin_dir is not writable, installing with sudo"
		sudo mkdir -p "$bin_dir"
		sudo install -m 755 "$unpacked/dfdu" "$bin_dir/dfdu"
		extras=0
	fi

	if installed="$("$bin_dir/dfdu" --version 2>&1)"; then
		ok "installed ${BOLD}$installed${RESET} to $(tilde "$bin_dir")/dfdu"
	else
		warn "installed $(tilde "$bin_dir")/dfdu, but it does not run: $installed"
	fi

	if [ "$extras" = 1 ]; then
		install_extras "$unpacked"
	fi

	case ":$PATH:" in
	*":$bin_dir:"*)
		# Another dfdu earlier on PATH (an older installer's, Homebrew's) would still win.
		found="$(command -v dfdu 2>/dev/null || true)"
		if [ -n "$found" ] && [ "$found" != "$bin_dir/dfdu" ]; then
			warn "$found comes first on your PATH, so 'dfdu' still runs that one. Remove it, or put $(tilde "$bin_dir") first."
		fi
		info "run ${BOLD}dfdu${RESET} to browse the current directory, ${BOLD}dfdu --help${RESET} for more"
		;;
	*) path_hint "$bin_dir" ;;
	esac
}

main "$@"
