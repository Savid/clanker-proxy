package inbox

import (
	"errors"
	"fmt"
	"net/http"
)

// Kind classifies an error for the API layer, which maps it to a status.
type Kind int

// Kinds. Errors without a Kind are internal.
const (
	KindInvalid Kind = iota + 1
	KindUnauthorized
	KindNotFound
	KindConflict
	KindUnprocessable
	KindUpstream
	KindTooMany
)

// Error is an error the caller caused, or another daemon did.
type Error struct {
	Kind Kind
	Msg  string
}

func (e *Error) Error() string {
	return e.Msg
}

func errorf(kind Kind, format string, args ...any) error {
	return &Error{Kind: kind, Msg: fmt.Sprintf(format, args...)}
}

// HTTPStatus is the status an API answers err with, and whether err is the
// caller's (or upstream's) fault rather than an internal failure.
func HTTPStatus(err error) (int, bool) {
	e, ok := errors.AsType[*Error](err)
	if !ok {
		return http.StatusInternalServerError, false
	}

	switch e.Kind {
	case KindInvalid:
		return http.StatusBadRequest, true
	case KindUnauthorized:
		return http.StatusUnauthorized, true
	case KindNotFound:
		return http.StatusNotFound, true
	case KindConflict:
		return http.StatusConflict, true
	case KindUnprocessable:
		return http.StatusUnprocessableEntity, true
	case KindUpstream:
		return http.StatusBadGateway, true
	case KindTooMany:
		return http.StatusTooManyRequests, true
	default:
		return http.StatusInternalServerError, false
	}
}
