#!/usr/bin/env bash
#
# Build Bone binaries for all platforms locally.
# Mirrors .github/workflows/build-binaries.yml
#
# Usage:
#   ./scripts/build-binaries.sh [--skip-install] [--skip-deps] [--skip-build] [--platform <platform>] [--out <dir>]
#
# Options:
#   --skip-install      Skip bun install
#   --skip-deps         Skip installing cross-platform dependencies
#   --skip-build        Skip bun run build
#   --platform <name>   Build only for specified platform (darwin-arm64, darwin-x64, linux-x64, linux-arm64, windows-x64, windows-arm64)
#   --out <dir>         Output directory (default: packages/coding-agent/binaries)
#
# Output:
#   packages/coding-agent/binaries/
#     bone-darwin-arm64
#     bone-darwin-x64
#     bone-linux-x64
#     bone-linux-arm64
#     bone-windows-x64.exe
#     bone-windows-arm64.exe

set -euo pipefail

cd "$(dirname "$0")/.."

SKIP_INSTALL=false
SKIP_DEPS=false
SKIP_BUILD=false
PLATFORM=""
OUTPUT_DIR=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --skip-install)
            SKIP_INSTALL=true
            shift
            ;;
        --skip-deps)
            SKIP_DEPS=true
            shift
            ;;
        --skip-build)
            SKIP_BUILD=true
            shift
            ;;
        --platform)
            PLATFORM="$2"
            shift 2
            ;;
        --out)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

# Validate platform if specified
if [[ -n "$PLATFORM" ]]; then
    case "$PLATFORM" in
        darwin-arm64|darwin-x64|linux-x64|linux-arm64|windows-x64|windows-arm64)
            ;;
        *)
            echo "Invalid platform: $PLATFORM"
            echo "Valid platforms: darwin-arm64, darwin-x64, linux-x64, linux-arm64, windows-x64, windows-arm64"
            exit 1
            ;;
    esac
fi

if [[ -z "$OUTPUT_DIR" ]]; then
    OUTPUT_DIR="packages/coding-agent/binaries"
fi
if [[ "$OUTPUT_DIR" != /* ]]; then
    OUTPUT_DIR="$(pwd)/$OUTPUT_DIR"
fi

if [[ "$SKIP_INSTALL" == "false" ]]; then
    echo "==> Installing dependencies..."
    bun install --ignore-scripts
else
    echo "==> Skipping bun install (--skip-install)"
fi

if [[ "$SKIP_DEPS" == "false" ]]; then
    echo "==> Installing cross-platform native bindings..."
    CLIPBOARD_VERSION=$(bun -e "console.log(require('./packages/images/package.json').dependencies['@mariozechner/clipboard'])")
    OPENTUI_VERSION=$(bun -e "console.log(require('./packages/tui/package.json').dependencies['@opentui/core'])")
    # Bun install only installs optional deps for the current platform. Cross-compilation
    # needs every clipboard binding and OpenTUI native library available to the bundler.
    # Use --force to bypass platform checks (os/cpu restrictions in package.json)
    # Install all in one command to avoid removing packages from previous installs
    bun add --no-save --force --ignore-scripts --os="*" --cpu="*" \
        @mariozechner/clipboard@"$CLIPBOARD_VERSION" \
        @mariozechner/clipboard-darwin-arm64@"$CLIPBOARD_VERSION" \
        @mariozechner/clipboard-darwin-x64@"$CLIPBOARD_VERSION" \
        @mariozechner/clipboard-linux-x64-gnu@"$CLIPBOARD_VERSION" \
        @mariozechner/clipboard-linux-arm64-gnu@"$CLIPBOARD_VERSION" \
        @mariozechner/clipboard-win32-x64-msvc@"$CLIPBOARD_VERSION" \
        @mariozechner/clipboard-win32-arm64-msvc@"$CLIPBOARD_VERSION" \
        @opentui/core-darwin-arm64@"$OPENTUI_VERSION" \
        @opentui/core-darwin-x64@"$OPENTUI_VERSION" \
        @opentui/core-linux-arm64@"$OPENTUI_VERSION" \
        @opentui/core-linux-x64@"$OPENTUI_VERSION" \
        @opentui/core-linux-arm64-musl@"$OPENTUI_VERSION" \
        @opentui/core-linux-x64-musl@"$OPENTUI_VERSION" \
        @opentui/core-win32-arm64@"$OPENTUI_VERSION" \
        @opentui/core-win32-x64@"$OPENTUI_VERSION"

    # Bun stores explicitly added platform packages without linking them into
    # the optional-dependency package directory. Link them there so dynamic
    # imports inside the package resolve for every cross-compiled target.
    link_bun_package() {
        local package_name="$1"
        local version="$2"
        local link_parent="$3"
        local name="${package_name#*/}"
        local package_dir
        package_dir=$(find "$(pwd)/node_modules" \( -type d -o -type l \) -name "$name" -print -quit)
        if [[ -z "$package_dir" ]]; then
            echo "Could not find installed package ${package_name}@${version}" >&2
            exit 1
        fi
        mkdir -p "$link_parent"
        ln -sfn "$package_dir" "$link_parent/$name"
    }

    opentui_core_dir=$(dirname "$(bun -e 'import { resolve } from "node:path"; console.log(Bun.resolveSync("@opentui/core", resolve(process.cwd(), "packages/tui")))')")
    for package_name in \
        @opentui/core-darwin-arm64 \
        @opentui/core-darwin-x64 \
        @opentui/core-linux-arm64 \
        @opentui/core-linux-x64 \
        @opentui/core-linux-arm64-musl \
        @opentui/core-linux-x64-musl \
        @opentui/core-win32-arm64 \
        @opentui/core-win32-x64; do
        link_bun_package "$package_name" "$OPENTUI_VERSION" "$opentui_core_dir/node_modules/@opentui"
    done

    clipboard_dir=$(dirname "$(bun -e 'import { resolve } from "node:path"; console.log(Bun.resolveSync("@mariozechner/clipboard", resolve(process.cwd(), "packages/images")))')")
    for package_name in \
        @mariozechner/clipboard-darwin-arm64 \
        @mariozechner/clipboard-darwin-x64 \
        @mariozechner/clipboard-linux-x64-gnu \
        @mariozechner/clipboard-linux-arm64-gnu \
        @mariozechner/clipboard-win32-x64-msvc \
        @mariozechner/clipboard-win32-arm64-msvc; do
        link_bun_package "$package_name" "$CLIPBOARD_VERSION" "$clipboard_dir/node_modules/@mariozechner"
    done
else
    echo "==> Skipping cross-platform native bindings (--skip-deps)"
fi

if [[ "$SKIP_BUILD" == "false" ]]; then
    echo "==> Building all packages..."
    bun run build
else
    echo "==> Skipping package build (--skip-build)"
fi

echo "==> Building binaries..."
cd packages/coding-agent

# Clean previous builds
rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR/entries"

# Determine which platforms to build
if [[ -n "$PLATFORM" ]]; then
    PLATFORMS=("$PLATFORM")
else
    PLATFORMS=(darwin-arm64 darwin-x64 linux-x64 linux-arm64 windows-x64 windows-arm64)
fi

for platform in "${PLATFORMS[@]}"; do
    echo "Building for $platform..."
    entrypoint="$OUTPUT_DIR/entries/$platform.ts"
    bun ../../scripts/generate-standalone-entry.mjs --platform "$platform" --out "$entrypoint"
    # Bun compiled executables only embed worker scripts when they are passed as
    # explicit build entrypoints. The runtime can still use new URL(...), but the
    # workers must be present in the compiled executable.
    if [[ "$platform" == windows-* ]]; then
		output_binary="$OUTPUT_DIR/bone-$platform.exe"
    else
		output_binary="$OUTPUT_DIR/bone-$platform"
    fi
	bun build --compile --target=bun-$platform "$entrypoint" ../images/src/image-resize-worker.ts ../memory/src/local-embedding-worker.ts ../memory/src/local-embedding-setup-worker.ts --outfile "$output_binary"
    case "$platform" in
        darwin-arm64) opentui_package="@opentui/core-darwin-arm64"; opentui_filename="libopentui.dylib" ;;
        darwin-x64) opentui_package="@opentui/core-darwin-x64"; opentui_filename="libopentui.dylib" ;;
        linux-arm64) opentui_package="@opentui/core-linux-arm64"; opentui_filename="libopentui.so" ;;
        linux-x64) opentui_package="@opentui/core-linux-x64"; opentui_filename="libopentui.so" ;;
        windows-arm64) opentui_package="@opentui/core-win32-arm64"; opentui_filename="opentui.dll" ;;
        windows-x64) opentui_package="@opentui/core-win32-x64"; opentui_filename="opentui.dll" ;;
    esac
    opentui_library=$(bun -e '
        import { dirname, resolve } from "node:path";
        const coreEntry = Bun.resolveSync("@opentui/core", resolve(process.cwd(), "../tui"));
        const entry = Bun.resolveSync(process.argv[1], dirname(coreEntry));
        console.log(resolve(dirname(entry), process.argv[2]));
    ' "$opentui_package" "$opentui_filename")
    bun ../../scripts/verify-opentui-standalone.mjs \
        --binary "$output_binary" \
        --native-library "$opentui_library" \
        --skip-run
done

# A successful compile does not prove that OpenTUI's file-imported dynamic
# library can be extracted from $bunfs and loaded. Exercise the host binary
# from an isolated directory whenever this build includes the host target.
HOST_PLATFORM=$(bun -e "console.log((process.platform === 'win32' ? 'windows' : process.platform) + '-' + process.arch)")
for platform in "${PLATFORMS[@]}"; do
    if [[ "$platform" == "$HOST_PLATFORM" ]]; then
        host_binary="$OUTPUT_DIR/bone-$platform"
        if [[ "$platform" == windows-* ]]; then
            host_binary="$OUTPUT_DIR/bone-$platform.exe"
        fi
        bun ../../scripts/verify-opentui-standalone.mjs --binary "$host_binary"
        isolated_directory=$(mktemp -d)
        isolated_binary="$isolated_directory/bone"
        if [[ "$platform" == windows-* ]]; then
            isolated_binary="$isolated_directory/bone.exe"
        fi
        cp "$host_binary" "$isolated_binary"
        chmod +x "$isolated_binary"
        BONE_RUNTIME_CACHE_DIR="$isolated_directory/cache" \
            BONE_VERIFY_STANDALONE_RUNTIME=1 \
            "$isolated_binary"
        test -f "$isolated_directory/cache/runtime/"*/.complete
        rm -rf "$isolated_directory"
    fi
done

rm -rf "$OUTPUT_DIR/entries"

echo ""
echo "==> Build complete!"
echo "Standalone binaries available in $OUTPUT_DIR/"
for platform in "${PLATFORMS[@]}"; do
    if [[ "$platform" == windows-* ]]; then
		ls -lh "$OUTPUT_DIR/bone-$platform.exe"
    else
		ls -lh "$OUTPUT_DIR/bone-$platform"
    fi
done
