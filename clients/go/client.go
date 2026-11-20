package popsub

import (
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"sync"
	"time"
)

// eventBuffer is how many events Client.Events holds for the application
// before the oldest is dropped.
const eventBuffer = 256

// Client is a connection to a PopSub server that reconnects on its own.
// Its methods are safe to call from several goroutines.
type Client struct {
	// engine owns the connection.
	engine *engine
	// cfg is the configuration, with defaults applied.
	cfg Config
	// mu guards subs.
	mu sync.Mutex
	// subs is the application's current subscription per topic, so
	// Unsubscribe can stop a delivery waiting on its full buffer.
	subs map[string]*Subscription
}

// Connect starts a client and waits for its first connection to log in
// and authenticate, retrying with the reconnect policy, for up to
// Config.RequestTimeout or until ctx ends.
//
// It fails with KindConfig for an invalid configuration, KindServer when
// the server refused the credentials, protocol version or session id (a
// rejected login is not retried), KindTimeout when no connection
// succeeded in time, and KindConnect when the reconnect policy ran out of
// attempts. When it fails, every goroutine it started has stopped before
// it returns.
func Connect(ctx context.Context, cfg Config) (*Client, error) {
	cfg, err := cfg.withDefaults()
	if err != nil {
		return nil, err
	}
	c := &Client{engine: newEngine(cfg), cfg: cfg, subs: make(map[string]*Subscription)}
	ready := c.engine.ready
	go c.engine.run()
	timer := time.NewTimer(cfg.RequestTimeout)
	defer timer.Stop()
	select {
	case failure := <-ready:
		if failure != nil {
			<-c.engine.done
			return nil, failure
		}
		return c, nil
	case <-timer.C:
		c.shutdown()
		<-c.engine.done
		return nil, timeoutError(nil)
	case <-ctx.Done():
		c.shutdown()
		<-c.engine.done
		return nil, timeoutError(ctx.Err())
	}
}

// Subscribe subscribes to a topic and waits for the server to confirm it.
// FromResume continues a durable session's stored subscription and
// replays nothing otherwise. Subscribing again to a topic replaces the
// earlier Subscription, whose channel is then closed. Subscribing again to
// a pattern does the same, and the topics already joined through it carry
// on from where each was on the new Subscription: nothing is replayed and
// nothing is skipped.
//
// On a durable session that an earlier process left subscriptions on, the
// server holds those topics until this call when it honours the client's
// request to, and FromResume then starts one past the session's cursor.
// When it does not, messages it sends for those topics before this call
// are not acknowledged, so they are replayed to the Subscription returned
// here. A
// pattern picks up every such topic it matches, each from where the
// session left it, whatever from says.
//
// The topic may be a pattern, in which + is any one whole level and a
// trailing # is the rest (orders/#, sensors/+/temp). The one Subscription
// then carries the messages of every matching topic the user may read,
// including topics created later, each under its own Topic and Seq. from
// applies to each topic that exists when subscribing; a topic created
// later is read from its first message. If the server ends one matching
// topic (see UnsubscribedEvent), only that topic stops. Unsubscribe with
// the pattern ends them all.
//
// It fails with KindServer when the server refused (for example
// not_authorized or invalid_topic), KindTimeout, or KindClosed.
func (c *Client) Subscribe(ctx context.Context, topic string, from StartFrom) (*Subscription, error) {
	return c.subscribe(ctx, topic, "", from)
}

// SubscribeShared joins a shared subscription: the members of group split
// the topic's messages between them, so this client receives only some,
// not in strict Seq order. Each (Topic, Seq) still reaches the application
// at most once per client, by remembering the last 4096 delivered seqs,
// but a message can reach a second member after the first disconnects
// without acknowledging it. from only decides where a group starts the
// first time it is used on a topic; after that the group's position is on
// the server, and the client rejoins with resume after a reconnect. The
// topic must be a plain name, not a pattern. Unsubscribe with the topic
// leaves the group.
//
// It fails with KindServer when the server refused (for example
// not_authorized, or bad_request for an invalid group or a pattern),
// KindTimeout, or KindClosed.
func (c *Client) SubscribeShared(ctx context.Context, topic, group string, from StartFrom) (*Subscription, error) {
	if group == "" {
		return nil, configError("a shared subscription needs a group")
	}
	return c.subscribe(ctx, topic, group, from)
}

// subscribe sends a subscribe, through a group when group is set, and
// waits for the server to confirm it.
func (c *Client) subscribe(ctx context.Context, topic, group string, from StartFrom) (*Subscription, error) {
	sub := newSubscription(topic, c.cfg.SubscriptionBuffer)
	reply := make(chan *Error, 1)
	if !c.engine.queue.push(subscribeCommand{topic: topic, from: from, group: group, sub: sub, reply: reply}) {
		return nil, c.closedErr()
	}
	failure, waitErr := waitFor(ctx, c, reply)
	if waitErr != nil {
		sub.halt()
		c.engine.queue.push(forgetCommand{topic: topic, sub: sub})
		return nil, waitErr
	}
	if failure != nil {
		return nil, failure
	}
	c.mu.Lock()
	c.subs[topic] = sub
	c.mu.Unlock()
	return sub, nil
}

// Unsubscribe unsubscribes from a topic, or from a pattern and every topic
// joined through it, and closes its Subscription's channel. A durable
// session forgets the stored subscriptions too.
//
// It fails with KindServer when the session store couldn't be updated,
// KindTimeout, or KindClosed.
func (c *Client) Unsubscribe(ctx context.Context, topic string) error {
	c.mu.Lock()
	if sub := c.subs[topic]; sub != nil {
		sub.halt()
		delete(c.subs, topic)
	}
	c.mu.Unlock()
	reply := make(chan *Error, 1)
	if !c.engine.queue.push(unsubscribeCommand{topic: topic, reply: reply}) {
		return c.closedErr()
	}
	failure, waitErr := waitFor(ctx, c, reply)
	if waitErr != nil {
		return waitErr
	}
	if failure != nil {
		return failure
	}
	return nil
}

// Publish publishes a message at QoS 0: sent once, with no confirmation.
// It does not wait. While disconnected, up to 10,000 such publishes are
// held and sent after reconnecting, the oldest dropped first. Failures
// the server reports arrive as ServerErrorEvent.
//
// It fails with KindClosed when the client has stopped.
func (c *Client) Publish(topic, payload string) error {
	return c.publish(topic, payload, "")
}

// PublishBytes publishes bytes at QoS 0, for a body that isn't text. They
// travel as standard padded base64 with the message's Encoding set to
// EncodingBase64, so they take about a third more room against the
// server's payload limit; subscribers read them back with Message.Bytes.
// The client keeps no reference to body after the call returns.
//
// Otherwise it behaves like Publish.
//
// It fails with KindClosed when the client has stopped.
func (c *Client) PublishBytes(topic string, body []byte) error {
	return c.publish(topic, base64.StdEncoding.EncodeToString(body), EncodingBase64)
}

// publish queues a QoS 0 publish of a payload already in its wire form,
// for Publish and PublishBytes.
func (c *Client) publish(topic, payload string, encoding Encoding) error {
	if !c.engine.queue.push(publishCommand{topic: topic, payload: payload, encoding: encoding}) {
		return c.closedErr()
	}
	return nil
}

// PublishConfirmed publishes a message at QoS 1 and waits until the server
// has stored it. The message gets a new UUID message_id and is resent
// under the same request id and message_id after a reconnect until
// confirmed, so the server stores it once.
//
// It fails with KindServer when the server refused (for example
// not_authorized, rate_limited or payload_too_large), KindTimeout (it may
// still have been stored), or KindClosed.
func (c *Client) PublishConfirmed(ctx context.Context, topic, payload string) (Published, error) {
	return c.publishConfirmed(ctx, topic, payload, "")
}

// PublishBytesConfirmed publishes bytes at QoS 1 and waits until the
// server has stored them, for a body that isn't text. They travel as
// standard padded base64 with the message's Encoding set to
// EncodingBase64, so they take about a third more room against the
// server's payload limit; subscribers read them back with Message.Bytes.
// The client keeps no reference to body after the call returns.
//
// Otherwise it behaves like PublishConfirmed, resends included.
//
// It fails with KindServer when the server refused (for example
// not_authorized, rate_limited or payload_too_large), KindTimeout (it may
// still have been stored), or KindClosed.
func (c *Client) PublishBytesConfirmed(ctx context.Context, topic string, body []byte) (Published, error) {
	return c.publishConfirmed(ctx, topic, base64.StdEncoding.EncodeToString(body), EncodingBase64)
}

// publishConfirmed queues a QoS 1 publish of a payload already in its wire
// form under a new message_id and waits for the server to confirm it, for
// PublishConfirmed and PublishBytesConfirmed.
func (c *Client) publishConfirmed(ctx context.Context, topic, payload string, encoding Encoding) (Published, error) {
	reply := make(chan publishResult, 1)
	command := publishCommand{topic: topic, payload: payload, encoding: encoding, messageID: newUUID(), reply: reply}
	if !c.engine.queue.push(command) {
		return Published{}, c.closedErr()
	}
	result, waitErr := waitFor(ctx, c, reply)
	if waitErr != nil {
		return Published{}, waitErr
	}
	if result.err != nil {
		return Published{}, result.err
	}
	return result.published, nil
}

// Admin returns the admin requests, sent through this client. They need
// the client to be logged in as an enabled admin.
func (c *Client) Admin() *Admin {
	return &Admin{client: c}
}

// Events returns the channel the client's events arrive on: connects,
// disconnects, truncations, stalls, token expiry and refresh, subscriptions
// the server ended, and server errors that answer no request. There is one
// channel per client, not one per call. It holds 256 events; when the
// application falls behind, the oldest are dropped. It is closed when the
// client stops.
func (c *Client) Events() <-chan Event {
	return c.engine.events
}

// Done returns a channel that is closed when the client has stopped for good.
func (c *Client) Done() <-chan struct{} {
	return c.engine.done
}

// Err returns nil while the client runs, and a KindClosed *Error carrying
// the close code that stopped it (0 when it was closed by the application
// or ran out of attempts) once it has stopped.
func (c *Client) Err() error {
	select {
	case <-c.engine.done:
		return closedError(c.engine.stopCode)
	default:
		return nil
	}
}

// Close closes the connection and stops the client, then waits until
// every goroutine the client started has exited. Commands queued before
// it are handled first; anything still waiting fails with KindClosed, and
// every Subscription's channel and the Events channel are closed. It sends
// a close frame and waits for the server's reply, for up to about ten
// seconds if the server doesn't answer. Calling it again does nothing and
// returns nil.
func (c *Client) Close() error {
	c.shutdown()
	<-c.engine.done
	return nil
}

// shutdown asks the engine to stop without waiting.
func (c *Client) shutdown() {
	c.engine.cancel()
	c.engine.queue.push(closeCommand{})
}

// request sends an admin request and waits for its reply. It is never
// queued while disconnected or resent after a reconnect.
func (c *Client) request(ctx context.Context, message any) (*serverFrame, error) {
	reply := make(chan requestResult, 1)
	if !c.engine.queue.push(requestCommand{message: message, reply: reply}) {
		return nil, c.closedErr()
	}
	result, waitErr := waitFor(ctx, c, reply)
	if waitErr != nil {
		return nil, waitErr
	}
	if result.err != nil {
		return nil, result.err
	}
	return result.frame, nil
}

// closedErr waits for the engine to finish stopping and returns the
// KindClosed error with the code that stopped it.
func (c *Client) closedErr() *Error {
	<-c.engine.done
	return closedError(c.engine.stopCode)
}

// waitFor waits for a command's outcome, up to the request timeout or
// until ctx ends.
func waitFor[T any](ctx context.Context, c *Client, reply <-chan T) (T, *Error) {
	var zero T
	timer := time.NewTimer(c.cfg.RequestTimeout)
	defer timer.Stop()
	select {
	case value := <-reply:
		return value, nil
	case <-timer.C:
		return zero, timeoutError(nil)
	case <-ctx.Done():
		return zero, timeoutError(ctx.Err())
	case <-c.engine.done:
		select {
		case value := <-reply:
			return value, nil
		default:
			return zero, closedError(c.engine.stopCode)
		}
	}
}

// newUUID returns a random (version 4) UUID, used as a confirmed publish's
// message_id.
func newUUID() string {
	var b [16]byte
	_, _ = rand.Read(b[:])
	b[6] = b[6]&0x0f | 0x40
	b[8] = b[8]&0x3f | 0x80
	var out [36]byte
	hex.Encode(out[0:8], b[0:4])
	out[8] = '-'
	hex.Encode(out[9:13], b[4:6])
	out[13] = '-'
	hex.Encode(out[14:18], b[6:8])
	out[18] = '-'
	hex.Encode(out[19:23], b[8:10])
	out[23] = '-'
	hex.Encode(out[24:36], b[10:16])
	return string(out[:])
}
