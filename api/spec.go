package api

import _ "embed"

// Spec is the OpenAPI document the daemon serves at /openapi.yaml.
//
//go:embed openapi.yaml
var Spec []byte
