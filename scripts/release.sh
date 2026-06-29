#!/usr/bin/env bash
set -euo pipefail

# ─── helpers ────────────────────────────────────────────────────────────────

BOLD='\033[1m'
BLUE='\033[0;34m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

STEP=0
TOTAL=10

info()  { echo -e "${BLUE}[INFO]${NC} $*"; }
step()  { STEP=$((STEP + 1)); echo; echo -e "${YELLOW}[${STEP}/${TOTAL}]${NC} ${BOLD}$*${NC}"; SECONDS=0; }
ok()    { echo -e "${GREEN}[ OK ]${NC} $* ($(format_time SECONDS))"; }
fail()  { echo -e "${RED}[FAIL]${NC} $*"; exit 1; }
skip()  { echo -e "${YELLOW}[SKIP]${NC} $*"; }

format_time() {
    local s=$1
    local m=$((s / 60))
    local r=$((s % 60))
    if ((m > 0)); then echo "${m}m${r}s"; else echo "${r}s"; fi
}

show_help() {
    cat <<'HELP'
Usage: scripts/release.sh [--dry-run] [--fork <remote>] <version>

Release automation for mac-screen-cast.

Arguments:
  <version>           Version in vX.Y.Z format (e.g., v0.2.0)

Options:
  --dry-run           Run all steps but skip actual publishing
  --fork <remote>     Push to <remote> instead of origin, skip cargo publish
  -h, --help          Show this help

Examples:
  scripts/release.sh --dry-run v0.2.0           # Validate the full pipeline
  scripts/release.sh --fork test-fork v0.2.0     # Test on a fork repository
  scripts/release.sh v0.2.0                      # Real release
HELP
}

# ─── arg parsing ────────────────────────────────────────────────────────────

DRY_RUN=false
FORK_MODE=false
FORK_REMOTE=""
REMOTE="origin"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run) DRY_RUN=true; shift ;;
        --fork) FORK_MODE=true; FORK_REMOTE="$2"; shift 2 ;;
        -h|--help) show_help; exit 0 ;;
        -*)
            if [[ "$1" =~ ^v[0-9] ]]; then break; fi
            echo "Unknown option: $1"; exit 1
            ;;
        *) VERSION="$1"; shift ;;
    esac
done

if [[ -z "${VERSION:-}" ]]; then
    echo "Error: version argument required"
    echo "Usage: scripts/release.sh [--dry-run] [--fork <remote>] <version>"
    exit 1
fi

if ! echo "$VERSION" | grep -qE '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
    echo "Error: version must match vX.Y.Z format (e.g., v0.2.0)"
    exit 1
fi

CARGO_VERSION="${VERSION#v}"

if $FORK_MODE && [[ -z "$FORK_REMOTE" ]]; then
    echo "Error: --fork requires a remote name argument"
    echo "Usage: scripts/release.sh --fork <remote> <version>"
    exit 1
fi

# ─── pre-checks ─────────────────────────────────────────────────────────────

echo -e "${BOLD}╔════════════════════════════════════════╗${NC}"
echo -e "${BOLD}║   mac-screen-cast Release Script      ║${NC}"
echo -e "${BOLD}║   Version: ${VERSION}${NC}"
if $DRY_RUN; then
    echo -e "${BOLD}║   Mode:    ${YELLOW}DRY RUN${NC}${BOLD}                     ║${NC}"
fi
if $FORK_MODE; then
    echo -e "${BOLD}║   Fork:    ${FORK_REMOTE}${BOLD}                     ║${NC}"
fi
echo -e "${BOLD}╚════════════════════════════════════════╝${NC}"

confirm() {
    if $DRY_RUN; then
        skip "Dry-run: skipping confirmation"
        return
    fi
    echo
    read -r -p "Proceed with release? [y/N] " reply
    if [[ ! "$reply" =~ ^[Yy]$ ]]; then
        echo "Aborted."
        exit 1
    fi
}

pre_check() {
    step "Pre-flight checks"

    # Git
    if [[ "$(git rev-parse --abbrev-ref HEAD)" != "master" ]]; then
        fail "Must be on master branch (currently on $(git rev-parse --abbrev-ref HEAD))"
    fi
    if ! git diff --quiet || ! git diff --cached --quiet; then
        fail "Working tree is not clean. Commit or stash changes first."
    fi

    # Tools
    command -v cargo >/dev/null 2>&1 || fail "cargo not found"
    command -v gh >/dev/null 2>&1 || fail "GitHub CLI (gh) not found. Install from https://cli.github.com"
    command -v cargo-audit >/dev/null 2>&1 || fail "cargo-audit not found. Run: cargo install cargo-audit --locked"

    # Git tag
    if git tag -l "$VERSION" | grep -q .; then
        fail "Tag $VERSION already exists"
    fi

    # gh auth
    if ! gh auth status 2>&1 | grep -q "Logged in"; then
        fail "gh CLI not authenticated. Run: gh auth login"
    fi

    # crates.io auth — skip in fork mode
    if ! $FORK_MODE; then
        if [[ -z "${CARGO_REGISTRY_TOKEN:-}" ]] && ! grep -q "token" ~/.cargo/credentials.toml 2>/dev/null; then
            fail "crates.io token not found. Run: cargo login or set CARGO_REGISTRY_TOKEN"
        fi
    fi

    # Remote
    if $FORK_MODE; then
        if ! git remote get-url "$FORK_REMOTE" >/dev/null 2>&1; then
            fail "Remote '$FORK_REMOTE' not found. Add it first: git remote add $FORK_REMOTE <url>"
        fi
    fi

    # Version check
    CURRENT_VERSION=$(grep '^version = ' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
    if [[ "$(printf '%s\n' "$CURRENT_VERSION" "$CARGO_VERSION" | sort -V | tail -1)" != "$CARGO_VERSION" ]]; then
        fail "New version $CARGO_VERSION must be greater than current version $CURRENT_VERSION"
    fi

    ok "All checks passed"
}

# ─── steps ──────────────────────────────────────────────────────────────────

step_test() {
    step "Run unit tests"
    cargo test
    ok "All tests passed"
}

step_audit() {
    step "Security audit"
    cargo audit
    ok "Security audit passed"
}

step_build() {
    step "Build release binary"
    cargo build --release
    ok "Build complete"
}

step_package() {
    step "Copy binary to packages/"
    local dest="packages/mac-screen-cast-${VERSION}"
    cp target/release/mac-screen-cast "$dest"
    chmod +x "$dest"
    ls -lh "$dest"
    ok "Binary copied to $dest"
}

step_version() {
    step "Update Cargo.toml version"
    if $DRY_RUN; then
        skip "Dry-run: would update version to $CARGO_VERSION"
        return
    fi
    sed -i '' "s/^version = \".*\"/version = \"$CARGO_VERSION\"/" Cargo.toml
    cargo check 2>/dev/null || true
    ok "Version updated to $CARGO_VERSION"
}

step_commit() {
    step "Commit and tag"
    if $DRY_RUN; then
        skip "Dry-run: would commit 'chore: release $VERSION' and tag $VERSION"
        return
    fi
    git add Cargo.toml Cargo.lock
    git commit -m "chore: release $VERSION"
    git tag "$VERSION"
    ok "Committed and tagged as $VERSION"
}

step_push() {
    local remote="$REMOTE"
    if $FORK_MODE; then
        remote="$FORK_REMOTE"
    fi

    step "Push to $remote"
    if $DRY_RUN; then
        skip "Dry-run: would push master and $VERSION to $remote"
        return
    fi
    git push "$remote" master
    git push "$remote" "$VERSION"
    ok "Pushed master and $VERSION to $remote"
}

step_publish_cratesio() {
    step "Publish to crates.io"
    if $DRY_RUN; then
        skip "Dry-run: would run cargo publish"
        return
    fi
    if $FORK_MODE; then
        skip "Fork mode: skipping cargo publish"
        return
    fi
    cargo publish
    ok "Published to crates.io"
}

step_release() {
    step "Create GitHub Release"
    if $DRY_RUN; then
        skip "Dry-run: would create GitHub Release $VERSION and upload target/release/mac-screen-cast"
        return
    fi

    local repo_args=""
    if $FORK_MODE; then
        local fork_url
        fork_url=$(git remote get-url "$FORK_REMOTE")
        # Strip https://github.com/ prefix and .git suffix
        local fork_repo
        fork_repo=$(echo "$fork_url" | sed 's|https://github.com/||; s|\.git$||')
        repo_args="--repo $fork_repo"
    fi

    # shellcheck disable=SC2086
    gh release create "$VERSION" \
        $repo_args \
        --title "$VERSION" \
        --generate-notes \
        target/release/mac-screen-cast
    ok "GitHub Release $VERSION created"
}

# ─── summary ────────────────────────────────────────────────────────────────

dry_run_summary() {
    echo
    echo -e "${BOLD}╔═══════════════════════════════════════╗${NC}"
    echo -e "${BOLD}║${NC}  ${GREEN}Dry-run complete.${NC}"
    echo -e "${BOLD}║${NC}  Would have:"
    echo -e "${BOLD}║${NC}    - Updated Cargo.toml to ${CARGO_VERSION}"
    echo -e "${BOLD}║${NC}    - Committed: chore: release ${VERSION}"
    echo -e "${BOLD}║${NC}    - Tagged: ${VERSION}"
    if $FORK_MODE; then
        echo -e "${BOLD}║${NC}    - Pushed: ${FORK_REMOTE} master + ${VERSION}"
    else
        echo -e "${BOLD}║${NC}    - Pushed: origin master + ${VERSION}"
    fi
    echo -e "${BOLD}║${NC}    - Published: crates.io"
    echo -e "${BOLD}║${NC}    - Created: GitHub Release ${VERSION}"
    echo -e "${BOLD}║${NC}    - Uploaded: target/release/mac-screen-cast"
    echo -e "${BOLD}╚═══════════════════════════════════════╝${NC}"
}

# ─── main ────────────────────────────────────────────────────────────────────

main() {
    pre_check
    step_test
    step_audit
    step_build
    step_package
    step_version
    step_commit
    step_push
    step_publish_cratesio
    step_release

    if $DRY_RUN; then
        dry_run_summary
    else
        echo
        echo -e "${GREEN}Release $VERSION complete!${NC}"
    fi
}

main
