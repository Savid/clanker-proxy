# syntax=docker/dockerfile:1

FROM golang:1.27.1 AS go
WORKDIR /src
COPY go.mod go.sum ./
RUN GOWORK=off go mod download
COPY api/ api/
COPY cmd/ cmd/
COPY cli/ cli/
COPY internal/ internal/
COPY pkg/ pkg/
ARG VERSION=dev
RUN GOWORK=off CGO_ENABLED=0 go build -trimpath -ldflags="-s -w -X main.version=${VERSION}" -o /out/cpd ./cmd/cpd && \
    GOWORK=off CGO_ENABLED=0 go build -trimpath -ldflags="-s -w -X main.version=${VERSION}" -o /out/cpctl ./cmd/cpctl && \
    mkdir /out/data

# Static binaries on distroless, running as nonroot. State lives in /data:
# mount a volume there. CP_DIR points both cpd and cpctl at it, so
# `docker exec <ctr> cpctl inbox` finds the owner token.
FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=go /out/cpd /out/cpctl /usr/local/bin/
# A new volume copies this directory's owner, so nonroot can write to it.
COPY --from=go --chown=65532:65532 /out/data /data
VOLUME /data
EXPOSE 8080
ENV CP_DIR=/data
ENV CP_NO_UPDATE_CHECK=1
ENTRYPOINT ["/usr/local/bin/cpd", "-listen", "0.0.0.0:8080"]
