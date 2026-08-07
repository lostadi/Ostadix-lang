#!/usr/bin/env bash

# check if the target directory argument was provided
if [ -z "$1" ]; then
    echo "error: please provide the path to your cloned repository."
    echo "usage: ./reclaim_commits.sh /path/to/O-lang"
    exit 1
fi

target_repository_path="$1"

# navigate into the directory or exit if it fails
cd "$target_repository_path" || {
    echo "error: could not navigate to directory $target_repository_path"
    exit 1
}

echo "starting the robust commit history rewrite for $target_repository_path..."

# use single quotes for env-filter to prevent premature shell expansion
# use case statements with wildcards to catch any bot name variations
git filter-branch -f --env-filter '
    new_author_name="leeostadi"
    new_author_email="ostadi.lee@gmail.com" 

    # check if the author contains copilot or claude (case-insensitive via wildcards)
    case "$GIT_AUTHOR_NAME" in
        *Copilot*|*copilot*|*Claude*|*claude*)
            export GIT_AUTHOR_NAME="$new_author_name"
            export GIT_AUTHOR_EMAIL="$new_author_email"
            ;;
    esac

    # check if the committer contains copilot or claude
    case "$GIT_COMMITTER_NAME" in
        *Copilot*|*copilot*|*Claude*|*claude*)
            export GIT_COMMITTER_NAME="$new_author_name"
            export GIT_COMMITTER_EMAIL="$new_author_email"
            ;;
    esac
' --tag-name-filter cat -- --branches --tags

# --- added retry logic (no original lines removed) ---
if [ $? -ne 0 ]; then
    # filter-branch failed – check if it was because of unstaged changes
    if git status --porcelain | grep -q .; then
        echo "Unstaged changes detected. Staging all changes and retrying..."
        git add -A
        # second attempt
        git filter-branch -f --env-filter '
            new_author_name="leeostadi"
            new_author_email="ostadi.lee@gmail.com" 

            case "$GIT_AUTHOR_NAME" in
                *Copilot*|*copilot*|*Claude*|*claude*)
                    export GIT_AUTHOR_NAME="$new_author_name"
                    export GIT_AUTHOR_EMAIL="$new_author_email"
                    ;;
            esac

            case "$GIT_COMMITTER_NAME" in
                *Copilot*|*copilot*|*Claude*|*claude*)
                    export GIT_COMMITTER_NAME="$new_author_name"
                    export GIT_COMMITTER_EMAIL="$new_author_email"
                    ;;
            esac
        ' --tag-name-filter cat -- --branches --tags
        if [ $? -ne 0 ]; then
            echo "Error: filter-branch failed even after staging changes."
            exit 1
        fi
    else
        echo "Error: filter-branch failed for unknown reasons (no unstaged changes found)."
        exit 1
    fi
fi
# --- end of added logic ---

echo "rewrite complete! run 'git log' to verify your new history."
echo "once you confirm it looks good, run: git push origin --force --all"
