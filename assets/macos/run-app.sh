#!/bin/bash
#
# What `cargo run` runs obs-rs through on macOS — see `.cargo/config.toml`.
#
# CEF is a framework only an application bundle carries, with its helper
# processes as applications beside it, so an executable run on its own has
# no browser engine. This puts the one cargo built into a bundle beside it —
# `target/<profile>/obs-rs.app`, made by `make-app.sh --dev`, which takes a
# fraction of a second once the framework is in — and runs the executable
# inside it.
#
# Runs it, rather than asking the system to open the bundle: the process is
# still this terminal's child, so its output is here, Ctrl+C stops it, and a
# permission it is given — screen recording, the camera — is the
# terminal's, which stays given however often obs-rs is rebuilt. Opened as
# an application it would be obs-rs asking, and an ad-hoc signed obs-rs is a
# new application to the system every build.
#
# Anything else cargo runs — a test binary, an example — is run as it is.

set -euo pipefail

executable=$1
shift
if [ "$(basename "$executable")" != "obs-rs" ]; then
    exec "$executable" "$@"
fi

here=$(cd "$(dirname "$0")" && pwd)
app="$(dirname "$executable")/obs-rs.app"
"$here/make-app.sh" --dev "$executable" "$app" > /dev/null
exec "$app/Contents/MacOS/obs-rs" "$@"
