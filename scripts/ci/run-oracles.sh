#!/usr/bin/env bash
# The three differential-oracle jobs of the old ci.yml, run on one shared build:
#   managed-oracle  (AssetStudio at $UNITY_RS_ORACLE_REPO, .NET 10)
#   audio-oracle    (vgmstream-cli $VGMSTREAM_VERSION)
#   unitypy-oracle  (UnityPy $UNITYPY_VERSION, maturin $MATURIN_VERSION)
# The toolchain is the rust-toolchain.toml channel (1.88.0), so no `cargo +1.88.0`.
set -euo pipefail
cd "$(dirname "$0")/../.."
: "${UNITY_RS_ORACLE_REPO:?}" "${VGMSTREAM_VERSION:?}" "${UNITYPY_VERSION:?}" "${MATURIN_VERSION:?}"

echo "::group::managed oracle"
dotnet restore oracle/AssetStudioOracle.csproj --ignore-failed-sources -p:NuGetAudit=false -p:AssetStudioRepo="$UNITY_RS_ORACLE_REPO"
cargo test -p unity-rs-core --test dotnet_oracle --locked -- --ignored --nocapture
python3 tools/test_monoschema.py
echo "::endgroup::"

echo "::group::vgmstream audio oracle"
curl --proto '=https' --proto-redir '=https' -fsSL -o vgmstream.zip \
  "https://github.com/vgmstream/vgmstream/releases/download/${VGMSTREAM_VERSION}/vgmstream-linux.zip"
mkdir -p "$HOME/.local/bin"
unzip -o -j vgmstream.zip vgmstream-cli -d "$HOME/.local/bin"
chmod +x "$HOME/.local/bin/vgmstream-cli"
export PATH="$HOME/.local/bin:$PATH"
# vgmstream exits 1 for its information modes; check the pinned version instead of `|| true`.
python3 -c "import json, os, subprocess; r = subprocess.run(['vgmstream-cli', '-V'], check=False, capture_output=True, text=True); assert r.returncode == 1, r; assert json.loads(r.stdout)['version'] == os.environ['VGMSTREAM_VERSION']"
cargo test -p unity-rs-core --lib --locked -- --ignored
echo "::endgroup::"

echo "::group::UnityPy oracle"
(
  cd crates/unity-rs-python
  python -m pip install "maturin==${MATURIN_VERSION}" "UnityPy==${UNITYPY_VERSION}"
  maturin build --release --locked --interpreter "$(command -v python)" --out dist
  python -c "import glob, subprocess, sys; w = glob.glob('dist/*.whl'); assert len(w) == 1, w; subprocess.check_call([sys.executable, '-m', 'pip', 'install', '--force-reinstall', '--no-deps', w[0]])"
  python -I tests/unitypy_oracle.py
)
echo "::endgroup::"
