// Command popsub-driver is the scenario driver for the Go client. It reads
// commands from standard input and writes responses and events to standard
// output, one JSON object per line, as clients/DRIVER.md in the PopSub
// repository describes. It holds at most one client and handles each
// command in its own goroutine, so responses can come out of order.
package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"sync"
	"time"

	popsub "github.com/soumyadepp/popsub/clients/go"
)

// command is one command line from the harness. Only the fields its op
// uses are set.
type command struct {
	// ID matches the response to the command.
	ID json.RawMessage `json:"id"`
	// Op names the command.
	Op string `json:"op"`
	// URL is the server's address, for connect.
	URL string `json:"url"`
	// Username is the login name, for connect.
	Username string `json:"username"`
	// Password is the password, for connect.
	Password string `json:"password"`
	// SessionID names a durable session, for connect; null is ephemeral.
	SessionID *string `json:"session_id"`
	// CleanStart discards the session's subscriptions, for connect.
	CleanStart bool `json:"clean_start"`
	// AckMode is "auto" or "manual", for connect.
	AckMode string `json:"ack_mode"`
	// Reconnect is the reconnect policy, for connect.
	Reconnect struct {
		// InitialMs is the first backoff ceiling in milliseconds.
		InitialMs int64 `json:"initial_ms"`
		// MaxMs is the largest backoff ceiling in milliseconds.
		MaxMs int64 `json:"max_ms"`
		// MaxAttempts gives up after this many attempts: null keeps trying,
		// and 0 turns reconnecting off (popsub.NoRetries).
		MaxAttempts *int `json:"max_attempts"`
	} `json:"reconnect"`
	// RequestTimeoutMs is the request timeout in milliseconds, for connect.
	RequestTimeoutMs int64 `json:"request_timeout_ms"`
	// Topic is the topic, for subscribe, unsubscribe, publish and ack.
	Topic string `json:"topic"`
	// From is the start point, for subscribe.
	From json.RawMessage `json:"from"`
	// Payload is the body, for publish and publish_confirmed.
	Payload string `json:"payload"`
	// Seq is the message to acknowledge, for ack.
	Seq uint64 `json:"seq"`
	// Method is the admin method, for admin.
	Method string `json:"method"`
	// Params are the admin method's parameters, for admin.
	Params json.RawMessage `json:"params"`
}

// adminParams are the parameters of every admin method, by their protocol names.
type adminParams struct {
	// Username is the account.
	Username string `json:"username"`
	// Password is a password, for create_user and set_password.
	Password string `json:"password"`
	// Role is a role, for create_user and set_role.
	Role popsub.Role `json:"role"`
	// Pattern is a topic pattern, for grant and revoke.
	Pattern string `json:"pattern"`
	// Access is what a grant allows.
	Access popsub.Access `json:"access"`
	// Enabled says whether the account may log in, for set_enabled.
	Enabled *bool `json:"enabled"`
}

// heldKey names a reported message by topic and seq.
type heldKey struct {
	// topic is the topic.
	topic string
	// seq is the seq.
	seq uint64
}

// failure is a command's error, written as the response's error object.
type failure map[string]any

// driver is the state kept between commands.
type driver struct {
	// outMu serialises writes to standard output.
	outMu sync.Mutex
	// out is where lines go.
	out io.Writer
	// mu guards the fields below.
	mu sync.Mutex
	// client is the client, once connected.
	client *popsub.Client
	// held are QoS 1 messages reported and not yet acknowledged.
	held map[heldKey]popsub.Message
	// current is the latest subscription per topic.
	current map[string]*popsub.Subscription
	// leaving are subscriptions the harness unsubscribed, whose end isn't reported.
	leaving map[*popsub.Subscription]bool
}

// main reads commands until standard input closes or exit, then closes
// the client.
func main() {
	d := &driver{
		out:     os.Stdout,
		held:    make(map[heldKey]popsub.Message),
		current: make(map[string]*popsub.Subscription),
		leaving: make(map[*popsub.Subscription]bool),
	}
	input := bufio.NewReader(os.Stdin)
	for {
		line, err := input.ReadBytes('\n')
		if len(line) > 0 && d.dispatch(line) {
			break
		}
		if err != nil {
			break
		}
	}
	d.mu.Lock()
	client := d.client
	d.mu.Unlock()
	if client != nil {
		client.Close()
	}
}

// dispatch parses one line and starts its command, reporting true for exit.
func (d *driver) dispatch(line []byte) bool {
	var c command
	if err := json.Unmarshal(line, &c); err != nil {
		fmt.Fprintf(os.Stderr, "popsub-driver: ignoring a line that isn't a command: %v\n", err)
		return false
	}
	if c.Op == "exit" {
		d.respond(c.ID, nil, nil)
		return true
	}
	go func() {
		result, fail := d.handle(&c)
		d.respond(c.ID, result, fail)
	}()
	return false
}

// write prints one JSON line.
func (d *driver) write(value any) {
	data, err := json.Marshal(value)
	if err != nil {
		fmt.Fprintf(os.Stderr, "popsub-driver: can't encode a line: %v\n", err)
		return
	}
	d.outMu.Lock()
	defer d.outMu.Unlock()
	_, _ = d.out.Write(append(data, '\n'))
}

// respond writes a command's response: ok with the result fields, or the error.
func (d *driver) respond(id json.RawMessage, result map[string]any, fail failure) {
	response := map[string]any{"id": id}
	if fail != nil {
		response["ok"] = false
		response["error"] = fail
	} else {
		for key, value := range result {
			response[key] = value
		}
		response["ok"] = true
	}
	d.write(response)
}

// handle carries out one command and returns its result fields or its error.
func (d *driver) handle(c *command) (map[string]any, failure) {
	if c.Op == "connect" {
		return d.connect(c)
	}
	d.mu.Lock()
	client := d.client
	d.mu.Unlock()
	if client == nil {
		return nil, plain("protocol", "connect first")
	}
	ctx := context.Background()
	switch c.Op {
	case "subscribe":
		return d.subscribe(ctx, client, c)
	case "unsubscribe":
		d.mu.Lock()
		if sub := d.current[c.Topic]; sub != nil {
			d.leaving[sub] = true
		}
		d.mu.Unlock()
		if err := client.Unsubscribe(ctx, c.Topic); err != nil {
			return nil, describe(err)
		}
		return nil, nil
	case "publish":
		if err := client.Publish(c.Topic, c.Payload); err != nil {
			return nil, describe(err)
		}
		return nil, nil
	case "publish_confirmed":
		published, err := client.PublishConfirmed(ctx, c.Topic, c.Payload)
		if err != nil {
			return nil, describe(err)
		}
		return map[string]any{"seq": published.Seq, "message_id": published.MessageID}, nil
	case "ack":
		key := heldKey{topic: c.Topic, seq: c.Seq}
		d.mu.Lock()
		message, ok := d.held[key]
		delete(d.held, key)
		d.mu.Unlock()
		if !ok {
			return nil, plain("protocol", "no such message was reported")
		}
		message.Ack()
		return nil, nil
	case "admin":
		return admin(ctx, client, c)
	case "close":
		client.Close()
		return nil, nil
	default:
		return nil, plain("protocol", "unknown op "+c.Op)
	}
}

// connect creates the client, waits for its first connection, and starts
// forwarding its events. DRIVER.md's max_attempts maps to
// ReconnectPolicy.MaxAttempts: null keeps trying (0), 0 is NoRetries, and
// a positive count passes through.
func (d *driver) connect(c *command) (map[string]any, failure) {
	cfg := popsub.Config{
		URL:         c.URL,
		Credentials: popsub.PasswordCredentials(c.Username, c.Password),
		CleanStart:  c.CleanStart,
		Reconnect: popsub.ReconnectPolicy{
			Initial: time.Duration(c.Reconnect.InitialMs) * time.Millisecond,
			Max:     time.Duration(c.Reconnect.MaxMs) * time.Millisecond,
		},
		RequestTimeout: time.Duration(c.RequestTimeoutMs) * time.Millisecond,
	}
	switch {
	case c.Reconnect.MaxAttempts == nil:
		cfg.Reconnect.MaxAttempts = 0
	case *c.Reconnect.MaxAttempts == 0:
		cfg.Reconnect.MaxAttempts = popsub.NoRetries
	default:
		cfg.Reconnect.MaxAttempts = *c.Reconnect.MaxAttempts
	}
	if c.SessionID != nil {
		cfg.SessionID = *c.SessionID
	}
	if c.AckMode == "manual" {
		cfg.AckMode = popsub.AckManual
	}
	client, err := popsub.Connect(context.Background(), cfg)
	if err != nil {
		return nil, describe(err)
	}
	d.mu.Lock()
	d.client = client
	d.mu.Unlock()
	go d.forwardEvents(client)
	return nil, nil
}

// subscribe subscribes and starts reporting the subscription's messages,
// and its end unless the harness unsubscribed.
func (d *driver) subscribe(ctx context.Context, client *popsub.Client, c *command) (map[string]any, failure) {
	from := popsub.FromResume
	if len(c.From) > 0 {
		if err := json.Unmarshal(c.From, &from); err != nil {
			return nil, plain("config", err.Error())
		}
	}
	sub, err := client.Subscribe(ctx, c.Topic, from)
	if err != nil {
		return nil, describe(err)
	}
	d.mu.Lock()
	d.current[c.Topic] = sub
	d.mu.Unlock()
	go d.forwardMessages(sub)
	return nil, nil
}

// forwardMessages reports each message of a subscription, holding QoS 1
// ones for ack, then reports the end of the subscription unless the
// harness unsubscribed from it.
func (d *driver) forwardMessages(sub *popsub.Subscription) {
	for message := range sub.Messages() {
		if message.QoS == 1 {
			d.mu.Lock()
			d.held[heldKey{topic: message.Topic, seq: message.Seq}] = message
			d.mu.Unlock()
		}
		d.write(map[string]any{
			"event":      "message",
			"topic":      message.Topic,
			"seq":        message.Seq,
			"payload":    message.Payload,
			"message_id": message.MessageID,
			"qos":        message.QoS,
		})
	}
	d.mu.Lock()
	leaving := d.leaving[sub]
	delete(d.leaving, sub)
	if d.current[sub.Topic()] == sub {
		delete(d.current, sub.Topic())
	}
	d.mu.Unlock()
	if !leaving {
		d.write(map[string]any{"event": "subscription_ended", "topic": sub.Topic()})
	}
}

// forwardEvents reports the client's events until it stops.
func (d *driver) forwardEvents(client *popsub.Client) {
	for event := range client.Events() {
		d.write(eventLine(event))
	}
}

// eventLine describes a client event as a driver event.
func eventLine(event popsub.Event) map[string]any {
	switch e := event.(type) {
	case popsub.ConnectedEvent:
		return map[string]any{"event": "connected", "resumed": e.Resumed}
	case popsub.DisconnectedEvent:
		return map[string]any{"event": "disconnected", "code": closeCode(e.Code), "reconnecting": e.Reconnecting}
	case popsub.TruncatedEvent:
		return map[string]any{"event": "truncated", "topic": e.Topic, "first_available": e.FirstAvailable}
	case popsub.StalledEvent:
		return map[string]any{"event": "stalled", "topic": e.Topic, "seq": e.Seq}
	case popsub.AuthExpiringEvent:
		return map[string]any{"event": "auth_expiring", "expires_at_ms": e.ExpiresAtMs}
	case popsub.AuthRefreshedEvent:
		return map[string]any{"event": "auth_refreshed"}
	case popsub.UnsubscribedEvent:
		return map[string]any{"event": "unsubscribed", "topic": e.Topic}
	case popsub.ServerErrorEvent:
		return map[string]any{"event": "server_error", "code": e.Code, "message": e.Message}
	default:
		return map[string]any{"event": fmt.Sprintf("%T", event)}
	}
}

// admin calls an admin method and returns {"result": ...}.
func admin(ctx context.Context, client *popsub.Client, c *command) (map[string]any, failure) {
	var params adminParams
	if len(c.Params) > 0 {
		if err := json.Unmarshal(c.Params, &params); err != nil {
			return nil, plain("config", err.Error())
		}
	}
	a := client.Admin()
	var result any
	var err error
	switch c.Method {
	case "create_user":
		result, err = a.CreateUser(ctx, params.Username, params.Password, params.Role)
	case "get_user":
		result, err = a.GetUser(ctx, params.Username)
	case "list_users":
		result, err = a.ListUsers(ctx)
	case "set_role":
		result, err = a.SetRole(ctx, params.Username, params.Role)
	case "grant":
		result, err = a.Grant(ctx, params.Username, params.Pattern, params.Access)
	case "revoke":
		result, err = a.Revoke(ctx, params.Username, params.Pattern)
	case "set_password":
		result, err = a.SetPassword(ctx, params.Username, params.Password)
	case "set_enabled":
		enabled := true
		if params.Enabled != nil {
			enabled = *params.Enabled
		}
		result, err = a.SetEnabled(ctx, params.Username, enabled)
	case "delete_user":
		err = a.DeleteUser(ctx, params.Username)
	default:
		return nil, plain("protocol", "unknown admin method "+c.Method)
	}
	if err != nil {
		return nil, describe(err)
	}
	return map[string]any{"result": result}, nil
}

// closeCode describes a close code for JSON: the number, or nil for none.
func closeCode(code popsub.CloseCode) any {
	if code == 0 {
		return nil
	}
	return int(code)
}

// describe turns a client error into the response's error object.
func describe(err error) failure {
	var e *popsub.Error
	if !errors.As(err, &e) {
		return plain("protocol", err.Error())
	}
	fail := plain(string(e.Kind), e.Error())
	switch e.Kind {
	case popsub.KindServer:
		fail["code"] = e.Code
	case popsub.KindClosed:
		fail["close_code"] = closeCode(e.CloseCode)
	}
	return fail
}

// plain builds an error object with a kind and a message.
func plain(kind, message string) failure {
	return failure{"kind": kind, "message": message}
}
