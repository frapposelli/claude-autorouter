# Build-only SDK; neither this image nor its package tools ship in AutoRouter.
ARG NODE_IMAGE
FROM ${NODE_IMAGE}
ARG APK_ARCH
ARG RUST_ARCHIVE_SHA256
COPY musl-builder-pins.json /opt/autorouter-build-inputs/sdk-pins.json
COPY fetch-musl-apks.mjs /tmp/fetch-musl-apks.mjs
RUN set -eux; \
    test "$(apk --print-arch)" = "$APK_ARCH"; \
    node --version | grep -qx v22.14.0; \
    mkdir -p /opt/autorouter-build-inputs /tmp/ci-packages; \
    node /tmp/fetch-musl-apks.mjs "$APK_ARCH" /opt/autorouter-build-inputs/sdk-pins.json \
      /tmp/ci-packages /opt/autorouter-build-inputs/apk-closed-inputs.json; \
    sha256sum -c /opt/autorouter-build-inputs/apk-package-expected.sha256; \
    sha256sum /tmp/ci-packages/*.apk > /opt/autorouter-build-inputs/apk-package-bytes.sha256; \
    apk --no-network --repositories-file /dev/null add /tmp/ci-packages/*.apk; \
    apk info -v | sort > /opt/autorouter-build-inputs/installed-packages.txt; \
    for package in \
      bash-5.2.37-r0 build-base-0.5-r3 ca-certificates-20260909-r0 curl-8.14.1-r3 git-2.47.3-r0 \
      openssl-3.3.7-r2 perl-5.40.4-r0 perl-dev-5.40.4-r0 perl-utils-5.40.4-r0 procps-ng-4.0.4-r2 tar-1.35-r2 xz-5.8.4-r0; do \
      grep -Fxq "$package" /opt/autorouter-build-inputs/installed-packages.txt; \
    done; \
    tar --version > /opt/autorouter-build-inputs/tar-version.txt; \
    grep -Fxq 'tar (GNU tar) 1.35' /opt/autorouter-build-inputs/tar-version.txt; \
    /usr/bin/xz --version > /opt/autorouter-build-inputs/xz-version.txt; \
    grep -Fxq 'xz (XZ Utils) 5.8.4' /opt/autorouter-build-inputs/xz-version.txt; \
    perl -MIPC::Cmd -MData::Dumper -MFile::Compare -MFile::Copy -e 'exit 0'; \
    mkdir /tmp/rust-bootstrap; \
    archive="rust-1.99.0-$APK_ARCH-unknown-linux-musl.tar.xz"; \
    curl --proto '=https' --tlsv1.2 --fail --show-error --location --max-time 300 \
      "https://static.rust-lang.org/dist/$archive" --output "/tmp/rust-bootstrap/$archive"; \
    printf '%s  %s\n' "$RUST_ARCHIVE_SHA256" "/tmp/rust-bootstrap/$archive" | sha256sum -c -; \
    printf '%s  %s\n' "$RUST_ARCHIVE_SHA256" "$archive" > /opt/autorouter-build-inputs/rust-archive.sha256; \
    tar --use-compress-program=/usr/bin/xz -xf "/tmp/rust-bootstrap/$archive" -C /tmp/rust-bootstrap; \
    bash "/tmp/rust-bootstrap/rust-1.99.0-$APK_ARCH-unknown-linux-musl/install.sh" \
      --prefix=/opt/autorouter-rust --disable-ldconfig --without=rust-docs; \
    /opt/autorouter-rust/bin/rustc --version | awk '$2 != "1.99.0" { exit 1 }'; \
    rm -rf /tmp/ci-packages /tmp/rust-bootstrap /tmp/fetch-musl-apks.mjs
ENV PATH=/opt/autorouter-rust/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin
