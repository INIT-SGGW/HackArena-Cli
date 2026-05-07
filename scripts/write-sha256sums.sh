#!/usr/bin/env sh

set -eu

PATH_ARG=""
VERSION_ARG=""
OUTPUT_FILE="SHA256SUMS.txt"

usage() {
    echo "Usage: $0 [--path <path> | --version <version>] [--output-file <name>]" >&2
    exit 1
}

get_hackarena_version() {
    cargo_toml_path=$1
    version_line=$(sed -n 's/^[[:space:]]*version[[:space:]]*=[[:space:]]*"\([^"]*\)".*$/\1/p' "$cargo_toml_path" | head -n 1)
    if [ -z "$version_line" ]; then
        echo "Could not find package version in $cargo_toml_path" >&2
        exit 1
    fi
    printf '%s\n' "$version_line"
}

hash_file() {
    target=$1
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$target" | awk '{print $1}'
        return
    fi
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$target" | awk '{print $1}'
        return
    fi
    if command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 "$target" | sed 's/^.*= //'
        return
    fi

    echo "No SHA256 tool found. Install sha256sum, shasum, or openssl." >&2
    exit 1
}

while [ $# -gt 0 ]; do
    case "$1" in
        --path)
            [ $# -ge 2 ] || usage
            PATH_ARG=$2
            shift 2
            ;;
        --version)
            [ $# -ge 2 ] || usage
            VERSION_ARG=$2
            shift 2
            ;;
        --output-file)
            [ $# -ge 2 ] || usage
            OUTPUT_FILE=$2
            shift 2
            ;;
        -h|--help)
            usage
            ;;
        *)
            usage
            ;;
    esac
done

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
CARGO_TOML_PATH="$REPO_ROOT/Cargo.toml"

if [ -n "$PATH_ARG" ] && [ -n "$VERSION_ARG" ]; then
    echo "Use either --path or --version, not both." >&2
    exit 1
fi

if [ -z "$PATH_ARG" ]; then
    if [ -z "$VERSION_ARG" ]; then
        VERSION_ARG=$(get_hackarena_version "$CARGO_TOML_PATH")
    fi
    PATH_ARG="$REPO_ROOT/deploy/$VERSION_ARG"
fi

case "$PATH_ARG" in
    /*) RESOLVED_INPUT=$PATH_ARG ;;
    [A-Za-z]:[\\/]*)
        RESOLVED_INPUT=$PATH_ARG
        ;;
    *)
        RESOLVED_INPUT="$REPO_ROOT/$PATH_ARG"
        ;;
esac

if [ -f "$RESOLVED_INPUT" ]; then
    parent_dir=$(dirname -- "$RESOLVED_INPUT")
    output_path="$parent_dir/$OUTPUT_FILE"
    file_name=$(basename -- "$RESOLVED_INPUT")
    hash=$(hash_file "$RESOLVED_INPUT")
    printf '%s  %s' "$hash" "$file_name" > "$output_path"
    echo "Wrote checksums to:"
    echo "  $output_path"
    exit 0
fi

if [ ! -d "$RESOLVED_INPUT" ]; then
    echo "Path does not exist: $RESOLVED_INPUT" >&2
    exit 1
fi

output_path="$RESOLVED_INPUT/$OUTPUT_FILE"
tmp_file=$(mktemp "${TMPDIR:-/tmp}/hackarena-sha256.XXXXXX")
trap 'rm -f "$tmp_file"' EXIT INT TERM HUP

find "$RESOLVED_INPUT" -maxdepth 1 -type f ! -name "$OUTPUT_FILE" -print | sort > "$tmp_file"

if [ ! -s "$tmp_file" ]; then
    echo "No files found in directory: $RESOLVED_INPUT" >&2
    exit 1
fi

: > "$output_path"
first_line=1
while IFS= read -r entry; do
    [ -n "$entry" ] || continue
    hash=$(hash_file "$entry")
    file_name=$(basename -- "$entry")
    if [ "$first_line" -eq 1 ]; then
        printf '%s  %s' "$hash" "$file_name" >> "$output_path"
        first_line=0
    else
        printf '\n%s  %s' "$hash" "$file_name" >> "$output_path"
    fi
done < "$tmp_file"

echo "Wrote checksums to:"
echo "  $output_path"
