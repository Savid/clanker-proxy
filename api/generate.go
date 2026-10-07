// Package api holds the OpenAPI spec, the source of truth for the HTTP API.
// The server and client in api/rest are generated from it.
package api

//go:generate go tool ogen -config ogen.yml -target rest -package rest -clean openapi.yaml
