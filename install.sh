#!/bin/sh
# Installs Hesychia, or updates it when run again: the sources go to ~/.hesychia/src, the
# compiler `hsy` to ~/.hesychia/bin, which bash and zsh find on their PATH.
#
#   chmod +x install.sh && ./install.sh
#
# HESYCHIA_HOME sets another folder than ~/.hesychia, HESYCHIA_REPOSITORY another repository.
set -eu

REPOSITORY="${HESYCHIA_REPOSITORY:-https://github.com/The-Hesychia-language/Hesychia.git}"
HOME_DIR="${HESYCHIA_HOME:-$HOME/.hesychia}"
SOURCES="$HOME_DIR/src"
BIN="$HOME_DIR/bin"
MARK="# Hesychia"

say() { printf '%s\n' "$*"; }
fail() { printf 'error: %s\n' "$*" >&2; exit 1; }

# What building Hesychia, and then building programs with it, needs.
command -v git >/dev/null 2>&1 || fail "git is needed: install it, then run this script again"
if ! command -v cargo >/dev/null 2>&1; then
    [ -x "$HOME/.cargo/bin/cargo" ] && PATH="$HOME/.cargo/bin:$PATH"
fi
command -v cargo >/dev/null 2>&1 || fail "Rust is needed to build hsy: install it from https://rustup.rs, then run this script again"
if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1 && ! command -v clang >/dev/null 2>&1; then
    case "$(uname -s)" in
        Darwin) fail "a C compiler is needed: run \`xcode-select --install\`, then this script again" ;;
        *) fail "a C compiler is needed: install gcc or clang (\`sudo apt install build-essential\` on Debian or Ubuntu), then run this script again" ;;
    esac
fi

# The sources: cloned the first time, brought up to date after.
if [ -d "$SOURCES/.git" ]; then
    say "updating Hesychia in $SOURCES"
    git -C "$SOURCES" fetch --quiet origin
    git -C "$SOURCES" reset --quiet --hard origin/HEAD
else
    say "downloading Hesychia into $SOURCES"
    rm -rf "$SOURCES"
    mkdir -p "$HOME_DIR"
    git clone --quiet "$REPOSITORY" "$SOURCES"
fi

say "building hsy (a few minutes the first time)"
cargo build --quiet --release --manifest-path "$SOURCES/Cargo.toml"

# The new binary replaces the old one.
mkdir -p "$BIN"
rm -f "$BIN/hsy"
cp "$SOURCES/target/release/hsy" "$BIN/hsy"
chmod +x "$BIN/hsy"

# Each shell finds hsy on its PATH, once: the line is added only if it is not there yet.
line="export PATH=\"$BIN:\$PATH\""
added=""
for rc in "$HOME/.bashrc" "$HOME/.zshrc"; do
    if ! grep -qs "$MARK" "$rc"; then
        printf '\n%s\n%s\n' "$MARK" "$line" >> "$rc"
        added="$added $rc"
    fi
done
# macOS opens bash as a login shell, which reads .bash_profile rather than .bashrc.
if [ "$(uname -s)" = Darwin ] && [ -f "$HOME/.bash_profile" ] && ! grep -qs "$MARK" "$HOME/.bash_profile"; then
    printf '\n%s\n%s\n' "$MARK" "$line" >> "$HOME/.bash_profile"
    added="$added $HOME/.bash_profile"
fi

say "installed $("$BIN/hsy" --version) in $BIN"
if [ -n "$added" ]; then
    say "added $BIN to PATH in:$added"
    say "open a new terminal, or run: export PATH=\"$BIN:\$PATH\""
fi
