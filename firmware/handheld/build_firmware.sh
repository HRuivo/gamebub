#!/usr/bin/env bash

set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

usage() {
    cat <<'EOF'
Usage: ./build_firmware.sh [BUILD_LABEL]

Build revision-4 firmware, generate a complete UF2, and archive all matching
debug artifacts. BUILD_LABEL defaults to a UTC timestamp plus the Git commit.

Optional environment variables:
  SYSTEM_DATA_DIR       System-data input (default: ./system-data)
  ARTIFACT_ROOT         Build archive root (default: ./debug-artifacts)
  ESPUP_EXPORT_FILE     espup environment file (default: ~/export-esp.sh)
  IDF_PATH              ESP-IDF tree (default: ~/.espressif/esp-idf/v5.4.3)
  IDF_PYTHON_BIN_DIR    ESP-IDF virtualenv bin directory (auto-detected)
  ALLOW_UNPINNED_TOOLCHAIN=1
                        Permit a compiler other than Rust 1.93 / LLVM 20
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi
if [[ $# -gt 1 ]]; then
    usage >&2
    exit 2
fi

git_commit="$(git rev-parse --short=12 HEAD)"
build_label="${1:-$(date -u +%Y%m%dT%H%M%SZ)-${git_commit}}"
if [[ ! "$build_label" =~ ^[A-Za-z0-9._-]+$ ]]; then
    echo "Invalid build label: use only letters, numbers, dot, underscore, and dash" >&2
    exit 2
fi

system_data_dir="${SYSTEM_DATA_DIR:-$SCRIPT_DIR/system-data}"
artifact_root="${ARTIFACT_ROOT:-$SCRIPT_DIR/debug-artifacts}"
artifact_dir="$artifact_root/$build_label"
espup_export_file="${ESPUP_EXPORT_FILE:-$HOME/export-esp.sh}"
export IDF_PATH="${IDF_PATH:-$HOME/.espressif/esp-idf/v5.4.3}"

if [[ -e "$artifact_dir" ]]; then
    echo "Artifact directory already exists: $artifact_dir" >&2
    exit 1
fi
mkdir -p "$artifact_dir"

build_complete=0
record_failure() {
    status=$?
    if [[ $status -ne 0 && $build_complete -eq 0 ]]; then
        printf 'Build failed with exit status %d\n' "$status" > "$artifact_dir/BUILD_FAILED"
        echo "Build failed; partial logs are in $artifact_dir" >&2
    fi
}
trap record_failure EXIT

if [[ ! -f "$espup_export_file" ]]; then
    echo "Missing espup environment file: $espup_export_file" >&2
    echo "Run: espup install --toolchain-version 1.93.0.0" >&2
    exit 1
fi
# shellcheck source=/dev/null
source "$espup_export_file"

if [[ ! -d "$IDF_PATH" ]]; then
    echo "ESP-IDF directory does not exist: $IDF_PATH" >&2
    exit 1
fi

idf_python_bin_dir="${IDF_PYTHON_BIN_DIR:-}"
if [[ -z "$idf_python_bin_dir" ]]; then
    preferred_python_dir="$HOME/.espressif/python_env/idf5.4_py3.12_env/bin"
    if [[ -x "$preferred_python_dir/python" ]]; then
        idf_python_bin_dir="$preferred_python_dir"
    else
        for candidate in "$HOME"/.espressif/python_env/idf5.4_*_env/bin; do
            if [[ -x "$candidate/python" ]]; then
                idf_python_bin_dir="$candidate"
                break
            fi
        done
    fi
fi
if [[ -z "$idf_python_bin_dir" || ! -x "$idf_python_bin_dir/python" ]]; then
    echo "Could not find the ESP-IDF 5.4 Python environment" >&2
    echo "Set IDF_PYTHON_BIN_DIR to its bin directory" >&2
    exit 1
fi
export PATH="$idf_python_bin_dir:$PATH"

required_commands=(cargo espflash git python python3 rustc sha256sum tar)
for command_name in "${required_commands[@]}"; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required command not found: $command_name" >&2
        exit 1
    fi
done

rust_version="$(rustc -Vv)"
if [[ "${ALLOW_UNPINNED_TOOLCHAIN:-0}" != "1" ]]; then
    if ! grep -q '^release: 1\.93\.0' <<<"$rust_version" || \
       ! grep -q '^LLVM version: 20\.' <<<"$rust_version"; then
        echo "Expected Rust 1.93 with LLVM 20; got:" >&2
        echo "$rust_version" >&2
        echo "Set ALLOW_UNPINNED_TOOLCHAIN=1 to override this check." >&2
        exit 1
    fi
fi

required_system_files=(
    boot.bit.hs
    fw_cart_backup.bin
    gameboy.bios-cgb.bin
    gameboy.bios-dmg.bin
    gameboy.bit.hs
    gba.bios.bin
    gba.bit.hs
)
for filename in "${required_system_files[@]}"; do
    if [[ ! -s "$system_data_dir/$filename" ]]; then
        echo "Missing or empty system-data file: $system_data_dir/$filename" >&2
        exit 1
    fi
done

{
    echo "Build label: $build_label"
    echo "Build time UTC: $(date -u --iso-8601=seconds)"
    echo "Repository: $(git rev-parse --show-toplevel)"
    echo "Git commit: $(git rev-parse HEAD)"
    echo "IDF_PATH: $IDF_PATH"
    echo "System data: $system_data_dir"
    echo
    rustc -Vv
    echo
    cargo -V
    espflash -V
    python -V
} > "$artifact_dir/build-info.txt"

git status --short > "$artifact_dir/git-status.txt"
git diff HEAD -- . > "$artifact_dir/source.patch"
sha256sum "${required_system_files[@]/#/$system_data_dir/}" \
    > "$artifact_dir/system-data.sha256"

source_inputs=(
    .cargo
    Cargo.lock
    Cargo.toml
    bindings_esp_tinyusb.h
    bootloader.bin
    build.rs
    components_esp32s3.lock
    espflash.toml
    generate_firmware_uf2.py
    partitions.csv
    res
    rust-toolchain.toml
    sdkconfig.defaults
    src
)
tar -czf "$artifact_dir/source-tree.tar.gz" "${source_inputs[@]}"

echo "Building revision-4 firmware..."
cargo build --release --features rev4 2>&1 | tee "$artifact_dir/cargo-build.log"

firmware_elf="$SCRIPT_DIR/target/xtensa-esp32s3-espidf/release/handheld"
if [[ ! -f "$firmware_elf" ]]; then
    echo "Build completed without producing the expected ELF: $firmware_elf" >&2
    exit 1
fi
cp "$firmware_elf" "$artifact_dir/handheld.elf"

uf2_name="gamebub-rev4_${build_label}.uf2"
echo "Generating $uf2_name..."
python3 generate_firmware_uf2.py \
    --firmware "$firmware_elf" \
    --system-data "$system_data_dir" \
    --hw-revision 4 \
    --output "$artifact_dir/$uf2_name" \
    2>&1 | tee "$artifact_dir/uf2-generation.log"

(
    cd "$artifact_dir"
    sha256sum handheld.elf "$uf2_name" source-tree.tar.gz system-data.sha256 \
        > SHA256SUMS
)

latest_link="$artifact_root/latest"
if [[ -e "$latest_link" && ! -L "$latest_link" ]]; then
    echo "Not updating $latest_link because it exists and is not a symlink" >&2
else
    ln -sfn "$build_label" "$latest_link"
fi

touch "$artifact_dir/BUILD_OK"
build_complete=1

echo
echo "Build complete"
echo "UF2: $artifact_dir/$uf2_name"
echo "ELF: $artifact_dir/handheld.elf"
echo "Checksums: $artifact_dir/SHA256SUMS"
