#!/usr/bin/env bash
# Writes the pre-publish review checklist (tarball contents + dependency diff
# vs. the currently-published version) to stdout. Used by publish-npm.yml
# (redirected into $GITHUB_STEP_SUMMARY) but runnable locally too, from
# programs/settlement/idl/client/js, to preview what a real publish would show:
#   ./scripts/publish-summary.sh [release-label]
set -euo pipefail

name=$(jq -r .name package.json)
version=$(jq -r .version package.json)
release_label="${1:-}"
release_label="${release_label:-$version (no release)}"

# Only the fields a reviewer cares about, pretty-printed so `diff` output below
# is one dependency per line rather than a single unreadable JSON blob.
dependency_fields() {
  jq -S '{dependencies, peerDependencies}'
}

cat <<EOF
## npm publish review

Before approving, confirm:
1. The tarball contents below only include expected files (no stray secrets, keys, or debug output).
2. The dependency diff below has no unexpected new/changed packages.
3. Package \`$name\` is being published at version \`$version\`, matching release \`$release_label\`.

### Tarball contents (\`npm pack --dry-run --ignore-scripts\`)
\`\`\`
$(npm pack --dry-run --ignore-scripts 2>&1)
\`\`\`

### Dependency diff vs. previously published version
EOF

view_err=$(mktemp)
if npm view "$name" --json >/dev/null 2>"$view_err"; then
  npm view "$name" --json | dependency_fields > /tmp/prev-deps.json
  dependency_fields < package.json > /tmp/curr-deps.json
  echo '```diff'
  diff -u /tmp/prev-deps.json /tmp/curr-deps.json || true
  echo '```'
elif grep -q "code E404" "$view_err"; then
  echo "_First publish of this package — nothing to diff against._"
else
  echo "::error::Failed to look up $name on the npm registry (not a 404, could be auth, network, or an outage)." >&2
  cat "$view_err" >&2
  exit 1
fi
