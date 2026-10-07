// Package apicontract reads operations from an OpenAPI spec so tests can
// check a handler against it: every operation is served (an operation ogen
// cannot generate, such as an event stream, that nobody hand-routed fails),
// and every operation admits exactly the callers its security declares.
package apicontract

import (
	"context"
	"encoding/json"
	"mime"
	"net/http"
	"net/http/httptest"
	"regexp"
	"slices"
	"strings"
	"testing"

	"github.com/go-faster/yaml"
)

// Operation is one operation in the spec.
type Operation struct {
	ID     string
	Method string
	// Path has path parameters filled with "abcd".
	Path string
	// Schemes are the security schemes that admit a caller; empty is public.
	Schemes []string
	// ContentType is the success response's, or "" for none.
	ContentType string
}

var pathParam = regexp.MustCompile(`\{[^}]+\}`)

type securityRequirements []map[string][]string

// Operations lists the spec's operations, each with its effective
// security: its own, or the spec's default.
func Operations(t *testing.T, spec []byte) []Operation {
	t.Helper()

	var doc struct {
		Security securityRequirements      `yaml:"security"`
		Paths    map[string]map[string]any `yaml:"paths"`
	}
	if err := yaml.Unmarshal(spec, &doc); err != nil {
		t.Fatal(err)
	}

	var ops []Operation

	for path, item := range doc.Paths {
		for method, raw := range item {
			op, isOp := raw.(map[string]any)
			if !isOp || method == "parameters" {
				continue
			}

			id, _ := op["operationId"].(string)
			o := Operation{
				ID: id, Method: strings.ToUpper(method), Path: pathParam.ReplaceAllString(path, "abcd"),
				Schemes: schemes(t, doc.Security, op), ContentType: successContentType(t, id, op),
			}
			ops = append(ops, o)
		}
	}

	if len(ops) == 0 {
		t.Fatal("spec has no operations")
	}

	slices.SortFunc(ops, func(a, b Operation) int { return strings.Compare(a.ID, b.ID) })

	return ops
}

func schemes(t *testing.T, global securityRequirements, op map[string]any) []string {
	t.Helper()

	reqs := global

	if raw, ok := op["security"]; ok {
		b, err := json.Marshal(raw)
		if err != nil {
			t.Fatal(err)
		}

		reqs = nil
		if err = json.Unmarshal(b, &reqs); err != nil {
			t.Fatal(err)
		}
	}

	var out []string

	for _, req := range reqs {
		for name := range req {
			out = append(out, name)
		}
	}

	slices.Sort(out)

	return out
}

// successContentType is the content type of the operation's 2xx response,
// or "" for one without a body.
func successContentType(t *testing.T, id string, op map[string]any) string {
	t.Helper()

	responses, _ := op["responses"].(map[string]any)
	for code, raw := range responses {
		if !strings.HasPrefix(code, "2") {
			continue
		}

		resp, _ := raw.(map[string]any)
		content, _ := resp["content"].(map[string]any)

		if len(content) > 1 {
			t.Fatalf("%s: want at most one %s content type, got %d", id, code, len(content))
		}

		for contentType := range content {
			return contentType
		}

		return ""
	}

	t.Fatalf("%s: no 2xx response", id)

	return ""
}

// Do sends op to h with an empty JSON body and the given bearer token ("" for
// none). An event stream's request is cancelled, so it ends after starting.
func Do(t *testing.T, h http.Handler, op Operation, token string) *httptest.ResponseRecorder {
	t.Helper()

	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()

	if op.ContentType == "text/event-stream" {
		cancel()
	}

	rec := httptest.NewRecorder()
	req := httptest.NewRequestWithContext(ctx, op.Method, op.Path, strings.NewReader("{}"))
	req.Header.Set("Content-Type", "application/json")

	if token != "" {
		req.Header.Set("Authorization", "Bearer "+token)
	}

	h.ServeHTTP(rec, req)

	return rec
}

// Routed reports why rec shows the operation was not routed to a handler:
// the router's 404 or 405, or a content type that is neither the declared
// one nor a problem. It returns "" when the operation was served.
func Routed(op Operation, rec *httptest.ResponseRecorder) string {
	got, _, _ := mime.ParseMediaType(rec.Header().Get("Content-Type"))
	emptyOK := op.ContentType == "" && rec.Code == http.StatusNoContent

	switch {
	case rec.Code == http.StatusMethodNotAllowed || routerNotFound(rec):
		return "not routed: " + rec.Body.String()
	case got != op.ContentType && got != "application/problem+json" && !emptyOK:
		return "content type " + got + ", want " + op.ContentType + " or a problem"
	default:
		return ""
	}
}

func routerNotFound(rec *httptest.ResponseRecorder) bool {
	if rec.Code != http.StatusNotFound {
		return false
	}

	var p struct {
		Detail string `json:"detail"`
	}

	_ = json.Unmarshal(rec.Body.Bytes(), &p)

	return p.Detail == "no such API route" || p.Detail == ""
}
