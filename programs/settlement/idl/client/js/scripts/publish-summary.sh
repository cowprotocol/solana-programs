#!/usr/bin/env bash
# Writes the pre-publish review checklist (tarball contents + dependency diff
# vs. the currently-published version) to stdout. Used by publish-npm.yml
# (redirected into $GITHUB_STEP_SUMMARY) but runnable locally too, from
# programs/settlement/idl/client/js, to preview what a real publish would show:
#   ./scripts/publish-summary.sh [release-label]
set -euo pipefail

name=$(node -p 'require("./package.json").name')
version=$(node -p 'require("./package.json").version')
release_label="${1:-$version (local run, no release)}"

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
if ! npm view "$name" version >/dev/null 2>"$view_err"; then
  if grep -q "code E404" "$view_err"; then
    echo "_First publish of this package — nothing to diff against._"
  else
    echo "::error::Failed to look up $name on the npm registry (not a 404, could be auth, network, or an outage)." >&2
    cat "$view_err" >&2
    exit 1
  fi
else
  prev_deps_json=$(npm view "$name" dependencies --json 2>/dev/null); [ -z "$prev_deps_json" ] && prev_deps_json='null'
  prev_peer_json=$(npm view "$name" peerDependencies --json 2>/dev/null); [ -z "$prev_peer_json" ] && prev_peer_json='null'
  node -e '
    const fs = require("fs");
    const [prevDeps, prevPeer] = process.argv.slice(1).map((s) => JSON.parse(s));
    const curr = require("./package.json");
    fs.writeFileSync("/tmp/prev-deps.json", JSON.stringify({dependencies: prevDeps, peerDependencies: prevPeer}, null, 2) + "\n");
    fs.writeFileSync("/tmp/curr-deps.json", JSON.stringify({dependencies: curr.dependencies ?? null, peerDependencies: curr.peerDependencies ?? null}, null, 2) + "\n");
  ' "$prev_deps_json" "$prev_peer_json"
  echo '```diff'
  diff -u /tmp/prev-deps.json /tmp/curr-deps.json || true
  echo '```'
fi
