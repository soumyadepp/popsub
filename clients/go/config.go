package popsub

import (
	"net/http"
	"net/url"
	"time"
)

// Defaults applied to zero fields of Config.
const (
	// DefaultRequestTimeout is the default Config.RequestTimeout.
	DefaultRequestTimeout = 30 * time.Second
	// DefaultSubscriptionBuffer is the default Config.SubscriptionBuffer.
	DefaultSubscriptionBuffer = 1024
	// DefaultReconnectInitial is the default ReconnectPolicy.Initial.
	DefaultReconnectInitial = 250 * time.Millisecond
	// DefaultReconnectMax is the default ReconnectPolicy.Max.
	DefaultReconnectMax = 30 * time.Second
)

// Credentials say how the client proves who it is: a username and password,
// or a token obtained elsewhere.
type Credentials struct {
	// Username is the username, for password credentials.
	Username string
	// Password is the password, for password credentials.
	Password string
	// Token is a JWT, used when Username is empty. The client can't refresh
	// it: when it expires the client stops with KindClosed, after an
	// AuthExpiring event.
	Token string
}

// PasswordCredentials builds password credentials. The client logs in on
// every connection and refreshes its token before it expires.
func PasswordCredentials(username, password string) Credentials {
	return Credentials{Username: username, Password: password}
}

// TokenCredentials builds credentials from a JWT obtained elsewhere.
func TokenCredentials(token string) Credentials {
	return Credentials{Token: token}
}

// usesPassword reports whether the client logs in with a username and
// password, and so can log in again after its token expires.
func (c Credentials) usesPassword() bool {
	return c.Username != ""
}

// AckMode says when QoS 1 messages are acknowledged.
type AckMode int

// The acknowledgement modes.
const (
	// AckAuto acknowledges a QoS 1 message as the client hands it to the
	// application. A message the application was processing when it
	// crashed is not redelivered.
	AckAuto AckMode = iota
	// AckManual acknowledges when the application calls Message.Ack. A
	// message never acknowledged is redelivered after a reconnect (to a
	// durable session), and stalls its subscription once the server runs
	// out of retries.
	AckManual
)

// ReconnectPolicy says how the client waits between reconnection attempts.
// The ceiling doubles from Initial up to Max, and each wait is a random
// time between zero and the ceiling, so clients reconnecting after a
// restart don't all arrive at once.
type ReconnectPolicy struct {
	// Initial is the ceiling before the first retry; 0 means
	// DefaultReconnectInitial.
	Initial time.Duration
	// Max is the largest ceiling; 0 means DefaultReconnectMax.
	Max time.Duration
	// MaxAttempts is how many reconnection attempts in a row may fail
	// before the client gives up and stops. 0 keeps trying forever.
	// NoRetries turns reconnecting off: a first connection that fails
	// fails Connect after one attempt, and a connection that drops later
	// stops the client with the close code that ended it. Other negative
	// values are a KindConfig error.
	MaxAttempts int
}

// NoRetries is the ReconnectPolicy.MaxAttempts value that turns
// reconnecting off, for callers that would rather fail fast, such as
// command-line tools. It matches max_attempts 0 in the Rust client.
const NoRetries = -1

// retries reports whether the policy allows reconnecting at all.
func (p ReconnectPolicy) retries() bool {
	return p.MaxAttempts != NoRetries
}

// exhausted reports whether attempt, the count of attempts that failed in
// a row, uses up the policy.
func (p ReconnectPolicy) exhausted(attempt int) bool {
	if !p.retries() {
		return attempt > 0
	}
	return p.MaxAttempts > 0 && attempt > p.MaxAttempts
}

// Ceiling returns the longest wait before an attempt, before jitter:
// Initial doubled attempt times, capped at Max. attempt counts the
// attempts that failed in a row, from 0.
func (p ReconnectPolicy) Ceiling(attempt int) time.Duration {
	if attempt < 0 {
		attempt = 0
	}
	if attempt > 30 {
		attempt = 30
	}
	ceiling := p.Initial
	for i := 0; i < attempt; i++ {
		if ceiling >= p.Max || ceiling > p.Max/2 {
			return p.Max
		}
		ceiling *= 2
	}
	if ceiling > p.Max {
		return p.Max
	}
	return ceiling
}

// Config is everything Connect needs.
type Config struct {
	// URL is the server's WebSocket URL, such as ws://127.0.0.1:8080, or
	// wss:// for TLS.
	URL string
	// Credentials say who to connect as.
	Credentials Credentials
	// SessionID names a durable session to hold, so subscriptions and
	// unacknowledged messages survive disconnects. Empty is an ephemeral
	// connection.
	SessionID string
	// CleanStart discards the session's stored subscriptions on the first
	// connection. It is never sent on reconnects.
	CleanStart bool
	// AckMode says when QoS 1 messages are acknowledged.
	AckMode AckMode
	// Reconnect says how to reconnect.
	Reconnect ReconnectPolicy
	// RequestTimeout is how long a request (connect, subscribe,
	// unsubscribe, confirmed publish, admin calls) may take, reconnects
	// included; 0 means DefaultRequestTimeout.
	RequestTimeout time.Duration
	// SubscriptionBuffer is how many messages each subscription buffers
	// for the application; 0 means DefaultSubscriptionBuffer. While one is
	// full the client stops reading the socket, and the server paces
	// delivery.
	SubscriptionBuffer int
	// HTTPClient, if set, is used for the WebSocket handshake, for example
	// to trust a private CA for wss://.
	HTTPClient *http.Client
}

// withDefaults returns the configuration with defaults in place of zero
// fields, or a KindConfig error when it can't work.
func (c Config) withDefaults() (Config, error) {
	u, err := url.Parse(c.URL)
	if err != nil {
		return c, configError("the URL doesn't parse: " + err.Error())
	}
	if u.Scheme != "ws" && u.Scheme != "wss" {
		return c, configError("the URL must start with ws:// or wss://")
	}
	if c.Credentials.Username == "" && c.Credentials.Token == "" {
		return c, configError("credentials need a username and password, or a token")
	}
	if c.Reconnect.Initial <= 0 {
		c.Reconnect.Initial = DefaultReconnectInitial
	}
	if c.Reconnect.Max <= 0 {
		c.Reconnect.Max = DefaultReconnectMax
	}
	if c.Reconnect.Max < c.Reconnect.Initial {
		c.Reconnect.Max = c.Reconnect.Initial
	}
	if c.Reconnect.MaxAttempts < NoRetries {
		return c, configError("Reconnect.MaxAttempts must be 0 (keep trying), a positive count, or NoRetries")
	}
	if c.RequestTimeout <= 0 {
		c.RequestTimeout = DefaultRequestTimeout
	}
	if c.SubscriptionBuffer <= 0 {
		c.SubscriptionBuffer = DefaultSubscriptionBuffer
	}
	return c, nil
}
