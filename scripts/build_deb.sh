#!/usr/bin/env bash
# Build Debian/Ubuntu packages from a release build.
#
#   scripts/build_deb.sh cli <version> <deb-arch> <rtlog-binary> <out-dir>
#       rtlog_<version>_<deb-arch>.deb: the rtlog CLI (a static musl binary,
#       so no dependencies), its man page, copyright and changelog.
#   scripts/build_deb.sh dev <version> <out-dir>
#       rtlog-dev_<version>_all.deb: c/include/rtlog.h and a pkg-config file.
#
# <version> is a release tag such as v0.2.5. The package version is the
# upstream version with Debian revision 1 (0.2.5-1); a value that does not
# start with a digit (a dry run's "dev") becomes 0~dev-1.
# Needs dpkg-deb and gzip. Run from the repository root.
set -euo pipefail

MAINTAINER="NieWil <58365598+NieWil@users.noreply.github.com>"
HOMEPAGE="https://github.com/NieWil/rtlog"

usage() {
    sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//' >&2
    exit 2
}

deb_version() {
    local v=${1#v}
    if [[ $v =~ ^[0-9] ]]; then echo "$v-1"; else echo "0~dev-1"; fi
}

# DEP-5 copyright file: MIT text in full, Apache-2.0 from common-licenses.
copyright() {
    cat <<EOF
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: rtlog
Upstream-Contact: $HOMEPAGE/issues
Source: $HOMEPAGE

Files: *
Copyright: 2026 The rtlog contributors
License: MIT or Apache-2.0

License: MIT
EOF
    # The MIT text without its title and copyright lines, indented as DEP-5 wants.
    tail -n +5 LICENSE-MIT | sed 's/^$/./; s/^/ /'
    cat <<'EOF'

License: Apache-2.0
 On Debian systems, the full text of the Apache License, Version 2.0 is in
 /usr/share/common-licenses/Apache-2.0.
EOF
}

# Minimal Debian changelog pointing at the upstream one.
changelog() {
    local pkg=$1 version=$2
    cat <<EOF | gzip -9n
$pkg ($version) unstable; urgency=medium

  * Upstream release ${version%-*}; see CHANGELOG.md upstream.

 -- $MAINTAINER  $(date -R -d "@${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}")
EOF
}

finish() {
    local root=$1 out=$2
    find "$root" -type d -exec chmod 0755 {} +
    (cd "$root" && find usr -type f -print0 | sort -z | xargs -0 md5sum) > "$root/DEBIAN/md5sums"
    chmod 0644 "$root/DEBIAN/md5sums"
    local size
    size=$(du -sk --exclude=DEBIAN "$root" | cut -f1)
    sed -i "s/@SIZE@/$size/" "$root/DEBIAN/control"
    mkdir -p "$(dirname "$out")"
    dpkg-deb --root-owner-group -Zxz --build "$root" "$out" >/dev/null
    echo "$out"
}

cmd=${1:-}
case $cmd in
cli)
    [ $# -eq 5 ] || usage
    version=$(deb_version "$2") arch=$3 bin=$4 outdir=$5
    root=$(mktemp -d)
    trap 'rm -rf "$root"' EXIT
    install -D -m 0755 "$bin" "$root/usr/bin/rtlog"
    install -d "$root/usr/share/man/man1" "$root/usr/share/doc/rtlog" "$root/DEBIAN"
    gzip -9n -c packaging/debian/rtlog.1 > "$root/usr/share/man/man1/rtlog.1.gz"
    copyright > "$root/usr/share/doc/rtlog/copyright"
    # A static-PIE musl binary looks like a shared library to lintian.
    install -d "$root/usr/share/lintian/overrides"
    printf '%s\n' "# usr/bin/rtlog is a static-PIE musl binary: no dynamic dependencies." \
        "rtlog: shared-library-lacks-prerequisites [usr/bin/rtlog]" \
        > "$root/usr/share/lintian/overrides/rtlog"
    changelog rtlog "$version" > "$root/usr/share/doc/rtlog/changelog.Debian.gz"
    cat > "$root/DEBIAN/control" <<EOF
Package: rtlog
Version: $version
Architecture: $arch
Maintainer: $MAINTAINER
Installed-Size: @SIZE@
Section: devel
Priority: optional
Homepage: $HOMEPAGE
Description: decoder for rtlog deferred binary logs
 rtlog is a deferred, lock-free logger for Rust, C, C++ and Zephyr: a log
 call writes a few binary bytes into a per-thread ring and the formatting
 happens on the host. This package contains the rtlog command, which turns
 a captured stream and its contract into text, JSON lines or a Perfetto
 trace, checks contracts, and extracts the contract from an ELF binary or
 firmware image.
EOF
    finish "$root" "$outdir/rtlog_${version}_${arch}.deb"
    ;;
dev)
    [ $# -eq 3 ] || usage
    version=$(deb_version "$2") outdir=$3
    root=$(mktemp -d)
    trap 'rm -rf "$root"' EXIT
    install -D -m 0644 c/include/rtlog.h "$root/usr/include/rtlog.h"
    install -d "$root/usr/share/pkgconfig" "$root/usr/share/doc/rtlog-dev" "$root/DEBIAN"
    cat > "$root/usr/share/pkgconfig/rtlog.pc" <<EOF
prefix=/usr
includedir=\${prefix}/include

Name: rtlog
Description: Header-only C/C++ producer for rtlog deferred binary logs
URL: $HOMEPAGE
Version: ${version%-*}
Cflags: -I\${includedir} -pthread
Libs: -pthread
EOF
    chmod 0644 "$root/usr/share/pkgconfig/rtlog.pc"
    copyright > "$root/usr/share/doc/rtlog-dev/copyright"
    changelog rtlog-dev "$version" > "$root/usr/share/doc/rtlog-dev/changelog.Debian.gz"
    cat > "$root/DEBIAN/control" <<EOF
Package: rtlog-dev
Version: $version
Architecture: all
Multi-Arch: foreign
Maintainer: $MAINTAINER
Installed-Size: @SIZE@
Section: libdevel
Priority: optional
Homepage: $HOMEPAGE
Suggests: rtlog
Description: header-only C/C++ producer for rtlog deferred binary logs
 rtlog is a deferred, lock-free logger: a log call writes a few binary
 bytes into a per-thread ring and the formatting happens on the host. This
 package contains rtlog.h, the single-header C and C++ implementation, and
 a pkg-config file (pkg-config --cflags rtlog). Decode captures with the
 rtlog command from the rtlog package.
EOF
    finish "$root" "$outdir/rtlog-dev_${version}_all.deb"
    ;;
*)
    usage
    ;;
esac
