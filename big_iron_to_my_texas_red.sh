#!/usr/bin/env bash
set -euo pipefail

REPO_NAME=""
VISIBILITY="--private"
SOLO_MODE=false
OVERWRITE=false
BACKUP=false

print_help() {
    cat <<'EOF_HELP'
big_iron_to_my_texas_red.sh

Purpose:
  Create a GitHub repo from the current directory with extra control flags.
  This is the heavier Texas Red workflow for creating, optionally replacing,
  and optionally scrubbing collaborators from a remote repo.

Usage:
  ./big_iron_to_my_texas_red.sh [options] <repo-name>
  ./big_iron_to_my_texas_red.sh --help

Options:
  --public      Create a public repository instead of a private one.
  --solo        Remove collaborators other than the authenticated owner.
  --overwrite   Delete or replace the remote repo if it already exists.
  --backup      When used with --overwrite, rename the old repo first.
  -h, --help    Show this help text.

What it does:
  1. Checks for git and gh, and requires gh auth.
  2. Initializes git if the current directory is not already a repo.
  3. Creates an initial or snapshot commit if needed.
  4. Creates the requested GitHub repo.
  5. Optionally backs up or deletes an existing remote.
  6. Optionally removes collaborators except you.
  7. Adds or updates origin and pushes the current branch.

Notes:
  - Without --overwrite, the script refuses to touch an existing remote.
  - With --backup, the previous remote repo is renamed instead of deleted.
EOF_HELP
}

usage() {
    print_help >&2
    exit 1
}

die() {
    echo ">> [!] Fatal: $*" >&2
    exit 1
}

require_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "Required command not found: $1"
}

ensure_gh_auth() {
    gh auth status >/dev/null 2>&1 || die "GitHub CLI is not authenticated. Run: gh auth login"
}

ensure_local_identity() {
    local login user_id user_name user_email

    login="$(gh api user --jq .login)"
    user_id="$(gh api user --jq .id)"
    user_name="$(gh api user --jq '.name // .login')"
    user_email="$(gh api user/public_emails --jq 'map(select(.primary == true and .verified == true))[0].email // empty')"
    if [ -z "$user_email" ]; then
        user_email="${user_id}+${login}@users.noreply.github.com"
    fi

    if ! git config user.name >/dev/null 2>&1; then
        git config user.name "$user_name"
    fi
    if ! git config user.email >/dev/null 2>&1; then
        git config user.email "$user_email"
    fi
}

ensure_local_repo() {
    if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        echo ">> [!] Local state untracked. Initializing Git object store..."
        git init
    fi
}

snapshot_if_needed() {
    git add -A

    if ! git rev-parse --verify HEAD >/dev/null 2>&1; then
        if git diff --cached --quiet; then
            git commit --allow-empty -m "Initial provisioning via texas_red_returns"
        else
            git commit -m "Initial provisioning via texas_red_returns"
        fi
        return
    fi

    if ! git diff --cached --quiet; then
        git commit -m "Snapshot via texas_red_returns"
    fi
}

current_branch() {
    local branch
    if branch="$(git symbolic-ref --quiet --short HEAD 2>/dev/null)" && [ -n "$branch" ]; then
        printf '%s\n' "$branch"
        return 0
    fi

    git branch -M main >/dev/null 2>&1
    printf 'main\n'
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --public) VISIBILITY="--public"; shift ;;
        --solo) SOLO_MODE=true; shift ;;
        --overwrite) OVERWRITE=true; shift ;;
        --backup) BACKUP=true; shift ;;
        -h|--help) print_help; exit 0 ;;
        -*) usage ;;
        *) REPO_NAME="$1"; shift ;;
    esac
done

[ -n "$REPO_NAME" ] || usage

require_cmd git
require_cmd gh
ensure_gh_auth
ensure_local_repo
ensure_local_identity
snapshot_if_needed

CURRENT_BRANCH="$(current_branch)"
GITHUB_USER="$(gh api user --jq .login)"
REPO_PATH="$GITHUB_USER/$REPO_NAME"

if gh repo view "$REPO_PATH" >/dev/null 2>&1; then
    if [ "$OVERWRITE" = true ]; then
        if [ "$BACKUP" = true ]; then
            STAMP="$(date +%s)"
            BACKUP_NAME="${REPO_NAME}_bak_${STAMP}"
            echo ">> [!] Conflict detected. Relocating existing repo to $BACKUP_NAME..."
            gh repo edit "$REPO_PATH" --rename "$BACKUP_NAME"
        else
            echo ">> [!] Overwrite enabled. Purging remote: $REPO_PATH"
            gh repo delete "$REPO_PATH" --confirm
        fi
    else
        die "Remote $REPO_PATH exists. Use --overwrite."
    fi
fi

echo ">> Provisioning $VISIBILITY infrastructure on GitHub..."
gh repo create "$REPO_PATH" "$VISIBILITY" --confirm

if [ "$SOLO_MODE" = true ]; then
    echo ">> [!] Solo mode engaged. Evicting non-owner entities..."
    COLLABS="$(gh api "repos/$REPO_PATH/collaborators" --jq ".[] | select(.login != \"$GITHUB_USER\") | .login")"
    if [ -n "$COLLABS" ]; then
        while IFS= read -r user; do
            [ -n "$user" ] || continue
            gh api -X DELETE "repos/$REPO_PATH/collaborators/$user"
            echo ">> Removed: $user"
        done <<EOF_COLLABS
$COLLABS
EOF_COLLABS
    fi
fi

NEW_URL="https://github.com/$REPO_PATH.git"
if git remote get-url origin >/dev/null 2>&1; then
    git remote set-url origin "$NEW_URL"
else
    git remote add origin "$NEW_URL"
fi

echo ">> Transporting DAG to $CURRENT_BRANCH..."
git push --set-upstream origin "$CURRENT_BRANCH"

echo ">> Subsystem clean. Repo: https://github.com/$REPO_PATH"
