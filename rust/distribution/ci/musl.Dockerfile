# Build-only SDK; neither this image nor its package tools ship in AutoRouter.
ARG NODE_IMAGE
FROM ${NODE_IMAGE}
ARG APK_ARCH
ARG APK_INDEX_SHA256
ARG RUST_ARCHIVE_SHA256
RUN set -eux; \
    test "$(apk --print-arch)" = "$APK_ARCH"; \
    node --version | grep -qx v22.14.0; \
    mkdir -p /opt/autorouter-build-inputs /tmp/ci-apk /tmp/ci-packages; \
    printf 'https://dl-cdn.alpinelinux.org/alpine/v3.21/main\n' > /tmp/ci-repositories; \
    apk --timeout 60 --cache-dir /tmp/ci-apk --repositories-file /tmp/ci-repositories update; \
    set -- /tmp/ci-apk/APKINDEX.*.tar.gz; test "$#" = 1; \
    printf '%s  %s\n' "$APK_INDEX_SHA256" "$1" | sha256sum -c -; \
    cp "$1" /opt/autorouter-build-inputs/APKINDEX.tar.gz; \
    apk --timeout 60 --cache-dir /tmp/ci-apk --repositories-file /tmp/ci-repositories fetch --recursive --output /tmp/ci-packages \
      bash build-base ca-certificates curl git openssl perl perl-dev perl-utils procps-ng xz; \
    printf '%s  %s\n' "$APK_INDEX_SHA256" "$1" | sha256sum -c -; \
    sha256sum /tmp/ci-packages/*.apk > /opt/autorouter-build-inputs/apk-package-bytes.sha256; \
    apk --no-network --repositories-file /dev/null add /tmp/ci-packages/*.apk; \
    apk info -v | sort > /opt/autorouter-build-inputs/installed-packages.txt; \
    for package in \
      bash-5.2.37-r0 build-base-0.5-r3 ca-certificates-20260909-r0 curl-8.14.1-r2 git-2.47.3-r0 \
      openssl-3.3.7-r2 perl-5.40.4-r0 perl-dev-5.40.4-r0 perl-utils-5.40.4-r0 procps-ng-4.0.4-r2 xz-5.8.4-r0; do \
      grep -Fxq "$package" /opt/autorouter-build-inputs/installed-packages.txt; \
    done; \
    perl -MIPC::Cmd -MData::Dumper -MFile::Compare -MFile::Copy -e 'exit 0'; \
    mkdir /tmp/rust-bootstrap; \
    archive="rust-1.99.0-$APK_ARCH-unknown-linux-musl.tar.xz"; \
    curl --proto '=https' --tlsv1.2 --fail --show-error --location --max-time 300 \
      "https://static.rust-lang.org/dist/$archive" --output "/tmp/rust-bootstrap/$archive"; \
    printf '%s  %s\n' "$RUST_ARCHIVE_SHA256" "/tmp/rust-bootstrap/$archive" | sha256sum -c -; \
    printf '%s  %s\n' "$RUST_ARCHIVE_SHA256" "$archive" > /opt/autorouter-build-inputs/rust-archive.sha256; \
    tar -xJf "/tmp/rust-bootstrap/$archive" -C /tmp/rust-bootstrap; \
    bash "/tmp/rust-bootstrap/rust-1.99.0-$APK_ARCH-unknown-linux-musl/install.sh" \
      --prefix=/opt/autorouter-rust --disable-ldconfig --without=rust-docs; \
    /opt/autorouter-rust/bin/rustc --version | awk '$2 != "1.99.0" { exit 1 }'; \
    rm -rf /tmp/ci-apk /tmp/ci-packages /tmp/rust-bootstrap /tmp/ci-repositories
ENV PATH=/opt/autorouter-rust/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin
