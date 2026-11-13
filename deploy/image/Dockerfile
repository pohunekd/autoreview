# The autoreview image for linux/amd64: the static release binary, gh and the
# pohunek CLI at pinned versions with checked sha256 sums, on a distroless base
# that runs as the non-root user 65532.
#
# The build context is a directory that holds the unpacked x86_64 release
# archive autoreview-<VERSION>-x86_64-unknown-linux-musl/; the image job of
# .github/workflows/release.yml builds it with BuildKit:
#
#   docker build -f deploy/image/Dockerfile --build-arg VERSION=<version> <context>
#
# Only linux/amd64: autoreview cannot review without the pohunek CLI, and pohunek
# publishes its Linux CLI for x86_64 only, so an arm64 image could not work.

FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2 AS tools
ARG TARGETARCH
ARG VERSION
# gh 2.104.0; the sum is the linux_amd64 line of gh_2.104.0_checksums.txt.
ADD https://github.com/cli/cli/releases/download/v2.104.0/gh_2.104.0_linux_amd64.tar.gz /work/gh.tar.gz
# pohunek 0.41.0, the POHUNEK_VERSION of scripts/setup; the sum is its .sha256 asset.
ADD https://github.com/zajca/pohunek/releases/download/v0.41.0/pohunek-cli-0.41.0-x86_64-unknown-linux-musl.tar.gz /work/pohunek.tar.gz
COPY autoreview-${VERSION}-x86_64-unknown-linux-musl/ /work/autoreview/
RUN set -eu; \
    [ "$TARGETARCH" = amd64 ] || { echo "only linux/amd64 is built, with BuildKit" >&2; exit 1; }; \
    [ -n "$VERSION" ] || { echo "--build-arg VERSION=<version> is required" >&2; exit 1; }; \
    cd /work; \
    printf '%s  %s\n' \
        98cb101ad87dbbd3f6b2f2bfcbd43197ccd07643140121d8a9cfaa91eadbc24e gh.tar.gz \
        52cca5250e9b07a3e9e54559f0adcd61e68b5181fcc3da3e99074bdbc841d304 pohunek.tar.gz \
        | sha256sum --check --strict; \
    test "$(autoreview/autoreview --version)" = "autoreview $VERSION"; \
    install -d -m 0755 /out/usr/bin /out/usr/local/bin /out/usr/share/doc/autoreview \
        /out/usr/share/doc/gh /out/usr/share/doc/pohunek; \
    install -d -m 0700 /out/var/lib/autoreview /out/run/autoreview; \
    tar -xzf gh.tar.gz; \
    tar -xzf pohunek.tar.gz; \
    install -m 0755 gh_2.104.0_linux_amd64/bin/gh /out/usr/bin/gh; \
    install -m 0755 pohunek-cli-0.41.0-x86_64-unknown-linux-musl/pohunek /out/usr/local/bin/pohunek; \
    install -m 0755 autoreview/autoreview /out/usr/local/bin/autoreview; \
    install -m 0644 autoreview/LICENSE /out/usr/share/doc/autoreview/LICENSE; \
    install -m 0644 gh_2.104.0_linux_amd64/LICENSE /out/usr/share/doc/gh/LICENSE; \
    install -m 0644 pohunek-cli-0.41.0-x86_64-unknown-linux-musl/LICENSE \
        /out/usr/share/doc/pohunek/LICENSE

FROM gcr.io/distroless/static-debian13:nonroot@sha256:aafe993665bf6200db38a02c11a9351e782a84b6a45e7f33b6695638c88e3931
ARG VERSION
LABEL org.opencontainers.image.source="https://github.com/pohunekd/autoreview" \
      org.opencontainers.image.description="autoreview pull request review service" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version="${VERSION}"
COPY --from=tools /out/usr/ /usr/
# The state and runtime directories belong to the user that runs the service: a
# new named volume copies the owner. A tmpfs over /run/autoreview needs its own
# uid, gid and mode options, as deploy/compose/compose.yml sets them.
COPY --from=tools --chown=65532:65532 /out/var/lib/autoreview /var/lib/autoreview
COPY --from=tools --chown=65532:65532 /out/run/autoreview /run/autoreview
# The paths of examples/config.yaml, so a config written by `autoreview init` runs
# unchanged; gh and pohunek keep their state under HOME, the state volume.
ENV AUTOREVIEW_CONFIG=/etc/autoreview/config.yaml \
    HOME=/var/lib/autoreview
USER 65532:65532
WORKDIR /var/lib/autoreview
VOLUME ["/var/lib/autoreview"]
ENTRYPOINT ["/usr/local/bin/autoreview"]
CMD ["serve"]
