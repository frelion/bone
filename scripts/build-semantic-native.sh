#!/usr/bin/env bash
# Build the platform-specific CrispEmbed/ggml libraries used by Bone's local
# semantic search through Bun FFI. This script is intentionally for release CI.

set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "Usage: $0 <darwin-arm64|darwin-x64|linux-x64|linux-arm64|win32-x64|win32-arm64>" >&2
    exit 2
fi

TARGET="$1"
case "$TARGET" in
    darwin-arm64|darwin-x64|linux-x64|linux-arm64|win32-x64|win32-arm64) ;;
    *)
        echo "Unsupported semantic native target: $TARGET" >&2
        exit 2
        ;;
esac

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SOURCE_DIR="${RUNNER_TEMP:-/tmp}/bone-crispembed-v0.15.0-${TARGET}"
BUILD_DIR="${SOURCE_DIR}/build"
INSTALL_DIR="${SOURCE_DIR}/install"
DESTINATION="${ROOT}/packages/coding-agent/native/${TARGET}"
CRISPEMBED_COMMIT="77f40f325747267fb633badcfe0118650a00e340"

if [[ -e "$SOURCE_DIR" ]]; then
    echo "Refusing to reuse existing native build directory: $SOURCE_DIR" >&2
    exit 1
fi

git clone --depth 1 --branch v0.15.0 --recurse-submodules \
    https://github.com/CrispStrobe/CrispEmbed.git "$SOURCE_DIR"

ACTUAL_COMMIT="$(git -C "$SOURCE_DIR" rev-parse HEAD)"
if [[ "$ACTUAL_COMMIT" != "$CRISPEMBED_COMMIT" ]]; then
    echo "Unexpected CrispEmbed revision: $ACTUAL_COMMIT" >&2
    exit 1
fi

git -C "$SOURCE_DIR" apply "${ROOT}/patches/crispembed-bone-mmap.patch"

cmake_args=(
    -S "$SOURCE_DIR"
    -B "$BUILD_DIR"
    -DCMAKE_BUILD_TYPE=Release
    -DGGML_METAL=OFF
    -DGGML_BLAS=OFF
    -DGGML_OPENMP=OFF
    -DGGML_LLAMAFILE=OFF
    -DCRISPEMBED_BUILD_SHARED=ON
    -DCRISPEMBED_NATIVE=ON
)

if [[ "$TARGET" == "win32-arm64" ]]; then
    # ggml's ARM backend rejects MSVC, but the Windows SDK and linker are
    # still provided by Visual Studio. ClangCL keeps that integration while
    # satisfying ggml's Clang requirement. This is cross-compilation from an
    # x64 runner, so `-mcpu=native` would target the host and is invalid for
    # clang-cl's ARM64 target; use ggml's portable ARM64 baseline instead.
    cmake_args+=( -A ARM64 -T ClangCL -DCRISPEMBED_NATIVE=OFF -DGGML_NATIVE=OFF )
elif [[ "$TARGET" == "win32-x64" ]]; then
    cmake_args+=( -A x64 )
fi

cmake "${cmake_args[@]}"
cmake --build "$BUILD_DIR" --config Release --parallel 4 \
    --target crispembed-shared crispembed-cli crispembed-server crispembed-quantize
cmake --install "$BUILD_DIR" --config Release --prefix "$INSTALL_DIR"

rm -rf "$DESTINATION"
mkdir -p "$DESTINATION"

copy_required() {
    local source="$1"
    local destination="$2"
    if [[ ! -e "$source" ]]; then
        echo "Expected native library is missing: $source" >&2
        exit 1
    fi
    cp -L "$source" "$DESTINATION/$destination"
}

case "$TARGET" in
    darwin-*)
        copy_required "$INSTALL_DIR/lib/libcrispembed.0.dylib" "libcrispembed.0.dylib"
        copy_required "$INSTALL_DIR/lib/libggml.0.dylib" "libggml.0.dylib"
        copy_required "$INSTALL_DIR/lib/libggml-cpu.0.dylib" "libggml-cpu.0.dylib"
        copy_required "$INSTALL_DIR/lib/libggml-base.0.dylib" "libggml-base.0.dylib"
        CRISPEMBED_LINK_LIBRARY="$INSTALL_DIR/lib/libcrispembed.0.dylib"
        ;;
    linux-*)
        copy_required "$INSTALL_DIR/lib/libcrispembed.so.0" "libcrispembed.so.0"
        copy_required "$INSTALL_DIR/lib/libggml.so.0" "libggml.so.0"
        copy_required "$INSTALL_DIR/lib/libggml-cpu.so.0" "libggml-cpu.so.0"
        copy_required "$INSTALL_DIR/lib/libggml-base.so.0" "libggml-base.so.0"
        CRISPEMBED_LINK_LIBRARY="$INSTALL_DIR/lib/libcrispembed.so.0"
        ;;
    win32-*)
        copy_required "$INSTALL_DIR/bin/crispembed.dll" "crispembed.dll"
        copy_required "$INSTALL_DIR/bin/ggml.dll" "ggml.dll"
        copy_required "$INSTALL_DIR/bin/ggml-cpu.dll" "ggml-cpu.dll"
        copy_required "$INSTALL_DIR/bin/ggml-base.dll" "ggml-base.dll"
        CRISPEMBED_LINK_LIBRARY="$INSTALL_DIR/lib/crispembed.lib"
        ;;
esac

printf '%s\n' "$CRISPEMBED_COMMIT" > "$DESTINATION/CRISPEMBED_COMMIT"
echo "Built $TARGET semantic Bun FFI libraries in $DESTINATION"
