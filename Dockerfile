# syntax=docker/dockerfile:1
# The release image: static cpd and cpctl from scripts/package.sh, on
# distroless, as nonroot. Package build/dist/linux-<arch>/ for each platform
# first (scripts/image.sh does, for this machine).

# Runs on the build machine, so a multi-platform build needs no emulation.
FROM --platform=$BUILDPLATFORM busybox:1.37@sha256:a7dbc319b1438115a207e11930dc4bb3e643dcb3ec0bdd694bb36cd8fad289ec AS data
RUN mkdir /data

FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab
ARG TARGETARCH
COPY --chmod=755 build/dist/linux-${TARGETARCH}/cpd build/dist/linux-${TARGETARCH}/cpctl /usr/local/bin/
# A new volume copies this directory's owner, so nonroot can write to it.
COPY --from=data --chown=65532:65532 /data /data
VOLUME /data
EXPOSE 18471
# cpd and cpctl both use it, so `docker exec <ctr> cpctl inbox` finds the
# owner token.
ENV CP_DIR=/data
ENTRYPOINT ["/usr/local/bin/cpd", "--listen", "0.0.0.0:18471"]
