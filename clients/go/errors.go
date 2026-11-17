package popsub

import (
	"context"
	"errors"
	"fmt"
	"time"
)

// ErrorKind says which of the six ways a client call failed.
type ErrorKind string

// The kinds of Error.
const (
	// KindServer means the server refused the request with an error frame;
	// Error.Code says why.
	KindServer ErrorKind = "server"
	// KindClosed means the client has stopped for good: Close was called,
	// the server closed the connection with a code that rules out
	// reconnecting, or the reconnect policy ran out of attempts.
	// Error.CloseCode holds the code that stopped it, or 0.
	KindClosed ErrorKind = "closed"
	// KindTimeout means the request did not complete within
	// Config.RequestTimeout, or its context ended first.
	KindTimeout ErrorKind = "timeout"
	// KindConnect means the server could not be reached, or the connection
	// dropped before an admin request's reply.
	KindConnect ErrorKind = "connect"
	// KindProtocol means the server sent something this client does not expect.
	KindProtocol ErrorKind = "protocol"
	// KindConfig means the configuration is invalid.
	KindConfig ErrorKind = "config"
)

// Sentinel errors, one per ErrorKind, for errors.Is. An *Error matches the
// sentinel of its Kind, so errors.Is(err, popsub.ErrClosed) reports whether
// the client has stopped.
var (
	// ErrServer matches errors of KindServer.
	ErrServer = errors.New("popsub: the server refused the request")
	// ErrClosed matches errors of KindClosed.
	ErrClosed = errors.New("popsub: the client is closed")
	// ErrTimeout matches errors of KindTimeout.
	ErrTimeout = errors.New("popsub: the request timed out")
	// ErrConnect matches errors of KindConnect.
	ErrConnect = errors.New("popsub: can't connect")
	// ErrProtocol matches errors of KindProtocol.
	ErrProtocol = errors.New("popsub: unexpected reply from the server")
	// ErrConfig matches errors of KindConfig.
	ErrConfig = errors.New("popsub: invalid configuration")
)

// sentinels maps each kind to its sentinel error.
var sentinels = map[ErrorKind]error{
	KindServer:   ErrServer,
	KindClosed:   ErrClosed,
	KindTimeout:  ErrTimeout,
	KindConnect:  ErrConnect,
	KindProtocol: ErrProtocol,
	KindConfig:   ErrConfig,
}

// ErrorCode is the code of a server error frame, such as "not_authorized".
type ErrorCode string

// The error codes of protocol version 1. A code this client does not know
// is reported as CodeInternal, as docs/DESIGN.md section 4 asks.
const (
	// CodeBadRequest is a malformed frame or an invalid argument.
	CodeBadRequest ErrorCode = "bad_request"
	// CodeUnauthenticated is a request sent before auth.
	CodeUnauthenticated ErrorCode = "unauthenticated"
	// CodeAuthFailed is a bad or expired token, or a bad session id, in auth.
	CodeAuthFailed ErrorCode = "auth_failed"
	// CodeInvalidCredentials is a wrong username or password in login.
	CodeInvalidCredentials ErrorCode = "invalid_credentials"
	// CodeLockedOut is too many failed logins; see Error.RetryAfter.
	CodeLockedOut ErrorCode = "locked_out"
	// CodeRateLimited is too many requests; see Error.RetryAfter.
	CodeRateLimited ErrorCode = "rate_limited"
	// CodeNotAuthorized is a topic the user may not use.
	CodeNotAuthorized ErrorCode = "not_authorized"
	// CodeRegistrationFailed is a refused register.
	CodeRegistrationFailed ErrorCode = "registration_failed"
	// CodeInvalidSessionID is an empty, too long or malformed session id.
	CodeInvalidSessionID ErrorCode = "invalid_session_id"
	// CodeSessionUnavailable is a session store the server could not reach.
	CodeSessionUnavailable ErrorCode = "session_unavailable"
	// CodeUnsupportedProtocol is a protocol version the server does not speak.
	CodeUnsupportedProtocol ErrorCode = "unsupported_protocol"
	// CodeStorageUnavailable is a message store the server could not reach.
	CodeStorageUnavailable ErrorCode = "storage_unavailable"
	// CodePayloadTooLarge is a payload over the server's limit.
	CodePayloadTooLarge ErrorCode = "payload_too_large"
	// CodeInvalidTopic is a topic name the server does not accept.
	CodeInvalidTopic ErrorCode = "invalid_topic"
	// CodeForbidden is an admin request from someone who is not an enabled admin.
	CodeForbidden ErrorCode = "forbidden"
	// CodeUserNotFound is an admin request naming a missing account.
	CodeUserNotFound ErrorCode = "user_not_found"
	// CodeUserExists is a create_user for a name that is taken.
	CodeUserExists ErrorCode = "user_exists"
	// CodeTopicNotFound is an admin request naming a topic nothing was
	// ever published to.
	CodeTopicNotFound ErrorCode = "topic_not_found"
	// CodeInternal is a failure inside the server, or a code this client
	// does not know.
	CodeInternal ErrorCode = "internal"
)

// knownCodes holds every ErrorCode this client knows, for normalizing
// unknown codes to CodeInternal.
var knownCodes = map[ErrorCode]bool{
	CodeBadRequest: true, CodeUnauthenticated: true, CodeAuthFailed: true,
	CodeInvalidCredentials: true, CodeLockedOut: true, CodeRateLimited: true,
	CodeNotAuthorized: true, CodeRegistrationFailed: true, CodeInvalidSessionID: true,
	CodeSessionUnavailable: true, CodeUnsupportedProtocol: true, CodeStorageUnavailable: true,
	CodePayloadTooLarge: true, CodeInvalidTopic: true, CodeForbidden: true,
	CodeUserNotFound: true, CodeUserExists: true, CodeTopicNotFound: true,
	CodeInternal: true,
}

// normalizeCode maps a code from the wire to an ErrorCode, treating codes
// this client does not know as CodeInternal.
func normalizeCode(code string) ErrorCode {
	if knownCodes[ErrorCode(code)] {
		return ErrorCode(code)
	}
	return CodeInternal
}

// CloseCode is a WebSocket close code the server sends. 0 means none: the
// connection ended without a close frame, or with a code PopSub does not use.
type CloseCode int

// The close codes a PopSub server sends, from docs/protocol/close-codes.json.
const (
	// CloseGoingAway is sent when the server shuts down.
	CloseGoingAway CloseCode = 1001
	// CloseMessageTooBig is sent for a frame over the server's size limit.
	CloseMessageTooBig CloseCode = 1009
	// CloseInternalError is sent when a handler failed unexpectedly.
	CloseInternalError CloseCode = 1011
	// CloseTryAgainLater is sent when the server is at its connection limit.
	CloseTryAgainLater CloseCode = 1013
	// CloseAuthFailed is sent for a bad token or session id in auth.
	CloseAuthFailed CloseCode = 4001
	// CloseUnauthenticated is sent for a request before auth.
	CloseUnauthenticated CloseCode = 4002
	// CloseSessionTakenOver is sent when another connection takes the session.
	CloseSessionTakenOver CloseCode = 4003
	// CloseTokenExpired is sent when the token expires without a new auth.
	CloseTokenExpired CloseCode = 4004
	// CloseIdleTimeout is sent when the server heard nothing for its idle timeout.
	CloseIdleTimeout CloseCode = 4005
	// CloseUnsupportedProtocol is sent for a protocol version the server does not speak.
	CloseUnsupportedProtocol CloseCode = 4006
	// CloseSessionUnavailable is sent when the session store is unreachable during auth.
	CloseSessionUnavailable CloseCode = 4007
	// CloseSlowConsumer is sent when the client's outgoing queue grew too large.
	CloseSlowConsumer CloseCode = 4008
	// CloseAccountDisabled is sent when an admin disabled or deleted the account.
	CloseAccountDisabled CloseCode = 4009
)

// closeReconnects says, for each known close code, whether a client should
// reconnect after it, from the last column of the table in docs/DESIGN.md
// section 4.4.
var closeReconnects = map[CloseCode]bool{
	CloseGoingAway: true, CloseMessageTooBig: true, CloseInternalError: true,
	CloseTryAgainLater: true, CloseAuthFailed: false, CloseUnauthenticated: true,
	CloseSessionTakenOver: false, CloseTokenExpired: true, CloseIdleTimeout: true,
	CloseUnsupportedProtocol: false, CloseSessionUnavailable: true,
	CloseSlowConsumer: true, CloseAccountDisabled: false,
}

// knownCloseCode maps a raw WebSocket status to a CloseCode, or 0 when it
// is not one PopSub sends.
func knownCloseCode(status int) CloseCode {
	if _, ok := closeReconnects[CloseCode(status)]; ok {
		return CloseCode(status)
	}
	return 0
}

// shouldReconnect says whether the close code allows reconnecting on its
// own. 0 (no code) allows it.
func (c CloseCode) shouldReconnect() bool {
	if c == 0 {
		return true
	}
	return closeReconnects[c]
}

// Error is the error every client call returns. Use errors.As to read it.
type Error struct {
	// Kind says how the call failed.
	Kind ErrorKind
	// Code is the server's error code, for KindServer.
	Code ErrorCode
	// Message describes the failure for people.
	Message string
	// RetryAfter is how long to wait before trying again, for
	// CodeLockedOut and CodeRateLimited; 0 otherwise.
	RetryAfter time.Duration
	// CloseCode is the close code that stopped the client, for KindClosed;
	// 0 when it was closed by the application or ran out of attempts.
	CloseCode CloseCode
	// cause is the underlying error, such as a context error or a network
	// error, if any.
	cause error
}

// Error describes the failure.
func (e *Error) Error() string {
	switch e.Kind {
	case KindServer:
		return fmt.Sprintf("popsub: the server refused the request (%s): %s", e.Code, e.Message)
	case KindClosed:
		if e.CloseCode != 0 {
			return fmt.Sprintf("popsub: the client is closed (close code %d)", e.CloseCode)
		}
		return "popsub: the client is closed"
	case KindTimeout:
		if e.Message != "" {
			return "popsub: the request timed out: " + e.Message
		}
		return "popsub: the request timed out"
	case KindConnect:
		if e.cause != nil {
			return "popsub: can't connect: " + e.Message + ": " + e.cause.Error()
		}
		return "popsub: can't connect: " + e.Message
	case KindProtocol:
		return "popsub: unexpected reply from the server: " + e.Message
	default:
		return "popsub: invalid configuration: " + e.Message
	}
}

// Unwrap returns the underlying error, if any: context.DeadlineExceeded or
// context.Canceled for timeouts, and the network or WebSocket error for
// connection failures.
func (e *Error) Unwrap() error {
	return e.cause
}

// Is reports whether target is the sentinel error of e's Kind, such as
// ErrClosed for KindClosed.
func (e *Error) Is(target error) bool {
	return target != nil && sentinels[e.Kind] == target
}

// Temporary reports whether retrying the same call later might work: true
// for timeouts, connection failures and the server's temporary refusals
// (rate_limited, locked_out, storage_unavailable, session_unavailable,
// internal).
func (e *Error) Temporary() bool {
	switch e.Kind {
	case KindTimeout, KindConnect:
		return true
	case KindServer:
		switch e.Code {
		case CodeRateLimited, CodeLockedOut, CodeStorageUnavailable, CodeSessionUnavailable, CodeInternal:
			return true
		}
	}
	return false
}

// serverError builds a KindServer error from an error frame.
func serverError(f *serverFrame) *Error {
	return &Error{
		Kind:       KindServer,
		Code:       normalizeCode(f.Code),
		Message:    f.Message,
		RetryAfter: time.Duration(f.RetryAfter) * time.Second,
	}
}

// closedError builds a KindClosed error.
func closedError(code CloseCode) *Error {
	return &Error{Kind: KindClosed, CloseCode: code}
}

// connectError builds a KindConnect error, wrapping cause when there is one.
func connectError(message string, cause error) *Error {
	return &Error{Kind: KindConnect, Message: message, cause: cause}
}

// protocolError builds a KindProtocol error.
func protocolError(message string) *Error {
	return &Error{Kind: KindProtocol, Message: message}
}

// configError builds a KindConfig error.
func configError(message string) *Error {
	return &Error{Kind: KindConfig, Message: message}
}

// timeoutError builds a KindTimeout error wrapping cause, a context error;
// a nil cause (the request timeout passed) becomes context.DeadlineExceeded.
func timeoutError(cause error) *Error {
	if cause == nil {
		cause = context.DeadlineExceeded
	}
	return &Error{Kind: KindTimeout, Message: cause.Error(), cause: cause}
}
