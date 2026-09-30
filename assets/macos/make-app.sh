#!/bin/bash
#
# Makes obs-rs.app out of a built executable: the macOS counterpart of the
# Linux archive's `$ORIGIN/lib` and the Windows archive's DLLs beside the
# executable.
#
#   assets/macos/make-app.sh target/release/obs-rs dist/obs-rs.app
#
# Worth running on a Mac of one's own too, not only by the release job: the
# system gives a permission — the camera, screen recording, Input Monitoring —
# to the application that asked for it, and an executable run from a terminal
# is the terminal asking. From the bundle it is obs-rs asking, under its own
# name.
#
# What it does:
#
# - Lays out the bundle: the executable, `Info.plist` with Cargo.toml's
#   version written in, the Korean usage strings, and an icon made from
#   `assets/obs-rs.png`.
# - Copies every library the executable loads that is not the system's —
#   FFmpeg's and whatever FFmpeg loads in turn, openh264 among them — into
#   `Contents/Frameworks`, found by following each one's load commands
#   rather than from a list kept by hand. Each is renamed `@rpath/<name>`,
#   every reference to it is rewritten to match, and the executable's search
#   path becomes that directory alone. That is what makes the bundle work
#   whatever FFmpeg was built with: vcpkg's already say `@rpath/...` and are
#   found through the build's rpath, while one built by hand names its
#   libraries by absolute path.
# - Signs every library and then the bundle, ad hoc. Rewriting a load
#   command invalidates the signature the linker gave it, and Apple silicon
#   runs nothing unsigned. An ad-hoc signature is not a developer's:
#   Gatekeeper still asks about a downloaded copy — see the README.
#
# Needs only what a Mac with the Command Line Tools has: otool,
# install_name_tool, codesign, plutil, sips and iconutil. Bash 3.2, which
# is the system's, so no associative arrays.

set -euo pipefail

if [ $# -ne 2 ]; then
    echo "usage: $0 <executable> <output.app>" >&2
    exit 2
fi

executable=$1
app=$2
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)
if [ -z "$version" ]; then
    echo "no version in $root/Cargo.toml" >&2
    exit 1
fi

rm -rf "$app"
contents="$app/Contents"
frameworks="$contents/Frameworks"
mkdir -p "$contents/MacOS" "$contents/Resources" "$frameworks"

binary="$contents/MacOS/obs-rs"
cp "$executable" "$binary"
chmod u+w "$binary"

cp "$here/Info.plist" "$contents/Info.plist"
plutil -replace CFBundleShortVersionString -string "$version" "$contents/Info.plist"
plutil -replace CFBundleVersion -string "$version" "$contents/Info.plist"
plutil -lint "$contents/Info.plist" > /dev/null
cp -R "$here/ko.lproj" "$contents/Resources/"

# The icon, at every size the source has pixels for. Finder draws the
# larger ones from the 256-pixel image.
iconset=$(mktemp -d)/obs-rs.iconset
mkdir -p "$iconset"
for size in 16 32 128 256; do
    sips -z "$size" "$size" "$root/assets/obs-rs.png" \
        --out "$iconset/icon_${size}x${size}.png" > /dev/null
done
for size in 16 128; do
    double=$((size * 2))
    cp "$iconset/icon_${double}x${double}.png" "$iconset/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$iconset" -o "$contents/Resources/obs-rs.icns"
rm -rf "$(dirname "$iconset")"

# A file's own search path, as its load commands state it.
rpaths_of() {
    otool -l "$1" | awk '$1 == "cmd" && $2 == "LC_RPATH" { getline; getline; print $2 }'
}

# Where an `@rpath/` reference resolves at build time: the executable's own
# search path, which is where the build found them.
rpaths=()
while IFS= read -r path; do
    rpaths+=("$path")
done < <(rpaths_of "$executable")

system_library() {
    case $1 in
        /System/* | /usr/lib/*) return 0 ;;
        *) return 1 ;;
    esac
}

# The file a reference names, from the directory the referring file came from
# and that file's own search path.
resolve() {
    local reference=$1 origin=$2 own=$3 name candidate
    case $reference in
        @rpath/*)
            name=${reference#@rpath/}
            for candidate in $own "${rpaths[@]+"${rpaths[@]}"}" "$origin"; do
                candidate=${candidate/#@loader_path/$origin}
                if [ -f "$candidate/$name" ]; then
                    echo "$candidate/$name"
                    return 0
                fi
            done
            ;;
        @loader_path/*)
            candidate="$origin/${reference#@loader_path/}"
            [ -f "$candidate" ] && { echo "$candidate"; return 0; }
            ;;
        @executable_path/*)
            candidate="$(dirname "$executable")/${reference#@executable_path/}"
            [ -f "$candidate" ] && { echo "$candidate"; return 0; }
            ;;
        /*)
            [ -f "$reference" ] && { echo "$reference"; return 0; }
            ;;
    esac
    return 1
}

# Each file still to look through, beside the directory it was copied from.
pending=("$binary")
origins=("$(dirname "$executable")")
while [ ${#pending[@]} -gt 0 ]; do
    file=${pending[0]}
    origin=${origins[0]}
    pending=("${pending[@]:1}")
    origins=("${origins[@]:1}")

    # What the file searches itself, which is where the build it came from
    # is — the linker records the directory it found each library in. Looked
    # through here, and then taken out: left in, it is searched before the
    # bundle, and on the machine that built it a library would load from
    # there instead. Paths with spaces in are not expected of a build tree.
    own=
    if [ "$file" != "$binary" ]; then
        own=$(rpaths_of "$file" | tr '\n' ' ')
    fi

    # A library's first entry is its own name, not something it loads.
    skip=1
    [ "$file" = "$binary" ] && skip=0
    while IFS= read -r reference; do
        if [ $skip -eq 1 ]; then
            skip=0
            continue
        fi
        system_library "$reference" && continue
        name=$(basename "$reference")
        if [ ! -f "$frameworks/$name" ]; then
            if ! source=$(resolve "$reference" "$origin" "$own"); then
                echo "$file loads $reference, which is nowhere to be found" >&2
                exit 1
            fi
            cp -L "$source" "$frameworks/$name"
            chmod u+w "$frameworks/$name"
            install_name_tool -id "@rpath/$name" "$frameworks/$name" 2> /dev/null
            pending+=("$frameworks/$name")
            origins+=("$(dirname "$source")")
        fi
        if [ "$reference" != "@rpath/$name" ]; then
            install_name_tool -change "$reference" "@rpath/$name" "$file" 2> /dev/null
        fi
    done < <(otool -L "$file" | tail -n +2 | awk '{ print $1 }')

    for path in $own; do
        install_name_tool -delete_rpath "$path" "$file" 2> /dev/null
    done
done

# The build's search path named the build machine's FFmpeg; the bundle's own
# is the only one that should be left.
for path in "${rpaths[@]+"${rpaths[@]}"}"; do
    install_name_tool -delete_rpath "$path" "$binary" 2> /dev/null
done
install_name_tool -add_rpath @executable_path/../Frameworks "$binary" 2> /dev/null

# `--force` because the linker signed each already; codesign says it is
# replacing that signature, which is the point, on every one.
for library in "$frameworks"/*.dylib; do
    codesign --force --sign - "$library" 2> /dev/null
done
codesign --force --sign - "$app" 2> /dev/null
codesign --verify --deep --strict "$app"

# Nothing may still point outside the bundle, by name or by search path:
# that is a library that works on this machine and nowhere else — or worse,
# works here from a copy the bundle does not carry.
for file in "$binary" "$frameworks"/*.dylib; do
    outside=$( (otool -L "$file" | tail -n +2 | awk '{ print $1 }'; rpaths_of "$file") |
        grep -v -e '^/System/' -e '^/usr/lib/' -e '^@rpath/' -e '^@executable_path/../Frameworks$' ||
        true)
    if [ -n "$outside" ]; then
        echo "$file still loads from outside the bundle:" >&2
        echo "$outside" >&2
        exit 1
    fi
done

echo "made $app ($version)"
