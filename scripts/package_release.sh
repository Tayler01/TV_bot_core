#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
output_root="${1:-dist/releases}"
output_dir="${repo_root}/${output_root}"
dashboard_dir="${repo_root}/apps/dashboard"

workspace_version="$(
  REPO_ROOT="${repo_root}" python3 - <<'PY'
import os
import pathlib
import re

content = pathlib.Path(os.environ["REPO_ROOT"], "Cargo.toml").read_text(encoding="utf-8")
match = re.search(r'(?m)^\s*version\s*=\s*"([^"]+)"\s*$', content)
if not match:
    raise SystemExit("Unable to determine workspace version from Cargo.toml")
print(match.group(1))
PY
)"

commit="$(git -C "${repo_root}" rev-parse --short HEAD)"
built_at_utc="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

os_name="$(uname -s)"
arch_name="$(uname -m)"
case "${os_name}" in
  Linux) platform="linux-${arch_name}" ;;
  Darwin) platform="macos-${arch_name}" ;;
  *)
    echo "Unsupported platform for package_release.sh: ${os_name}" >&2
    exit 1
    ;;
esac

bundle_name="tv-bot-core-${workspace_version}-${platform}-${commit}"
bundle_dir="${output_dir}/${bundle_name}"
archive_path="${output_dir}/${bundle_name}.tar.gz"

rm -rf "${bundle_dir}"
rm -f "${archive_path}"
mkdir -p "${bundle_dir}/bin" "${bundle_dir}/dashboard" "${bundle_dir}/config" "${bundle_dir}/docs/ops" "${bundle_dir}/strategies/examples"

pushd "${repo_root}" >/dev/null
cargo build --release -p tv-bot-runtime -p tv-bot-cli
popd >/dev/null

pushd "${dashboard_dir}" >/dev/null
npm ci
npm run build
popd >/dev/null

cp "${repo_root}/target/release/tv-bot-runtime" "${bundle_dir}/bin/tv-bot-runtime"
cp "${repo_root}/target/release/tv-bot-cli" "${bundle_dir}/bin/tv-bot-cli"
cp -R "${repo_root}/apps/dashboard/dist/." "${bundle_dir}/dashboard/"
cp "${repo_root}/config/runtime.example.toml" "${bundle_dir}/config/runtime.example.toml"
cp "${repo_root}/README.md" "${bundle_dir}/README.md"
cp "${repo_root}/LICENSE" "${bundle_dir}/LICENSE"
cp "${repo_root}/STRATEGY_SPEC.md" "${bundle_dir}/STRATEGY_SPEC.md"
cp -R "${repo_root}/docs/ops/." "${bundle_dir}/docs/ops/"
cp -R "${repo_root}/strategies/examples/." "${bundle_dir}/strategies/examples/"

cat > "${bundle_dir}/release-manifest.json" <<EOF
{
  "package_name": "tv-bot-core",
  "version": "${workspace_version}",
  "git_commit": "${commit}",
  "built_at_utc": "${built_at_utc}",
  "platform": "${platform}",
  "contents": {
    "runtime_binary": "bin/tv-bot-runtime",
    "cli_binary": "bin/tv-bot-cli",
    "dashboard": "dashboard",
    "runtime_config": "config/runtime.example.toml",
    "ops_docs": "docs/ops",
    "strategy_examples": "strategies/examples"
  }
}
EOF

tar -C "${output_dir}" -czf "${archive_path}" "${bundle_name}"
echo "Created release bundle at ${archive_path}"
