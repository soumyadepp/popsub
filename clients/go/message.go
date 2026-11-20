package popsub

import (
	"encoding/base64"
	"sync"
)

// Encoding says how a message's Payload string encodes its bytes. The
// zero value means the payload is text.
type Encoding string

// EncodingBase64 marks a payload that is standard padded base64 of the
// message's bytes, as PublishBytes and PublishBytesConfirmed send it.
const EncodingBase64 Encoding = "base64"

// Message is a message delivered on a subscription.
type Message struct {
	// Topic is the topic it was published to.
	Topic string
	// Seq is its position in the topic. Each (Topic, Seq) reaches the
	// application at most once per client.
	Seq uint64
	// MessageID is the publisher's id for it.
	MessageID string
	// Payload is the body as it travelled: the text itself, or base64 when
	// Encoding says so. Bytes decodes it either way.
	Payload string
	// Encoding is how Payload encodes the message's bytes: empty for text,
	// EncodingBase64 for a message published as bytes.
	Encoding Encoding
	// QoS is 0 or 1.
	QoS uint8
	// PublishedAtMs is when it was published, in milliseconds since the
	// Unix epoch.
	PublishedAtMs int64
	// ack sends the acknowledgement, for QoS 1 messages in AckManual; nil
	// otherwise.
	ack func()
}

// Bytes returns the message's bytes: the decoded Payload when Encoding is
// EncodingBase64, and the Payload's UTF-8 bytes otherwise. Each call
// returns a new slice.
//
// It fails with KindProtocol when the payload says it is base64 and is not
// standard padded base64. The server checks this on publish, so it means
// the record was stored some other way. The message was still delivered,
// and Payload holds the string as it arrived.
func (m Message) Bytes() ([]byte, error) {
	if m.Encoding != EncodingBase64 {
		return []byte(m.Payload), nil
	}
	decoded, err := base64.StdEncoding.Strict().DecodeString(m.Payload)
	if err != nil {
		return nil, protocolError("the payload isn't valid base64: " + err.Error())
	}
	return decoded, nil
}

// Ack acknowledges the message, so the server stops resending it and a
// durable session's cursor can move past it. It does nothing for QoS 0
// messages and in AckAuto, where the client already acknowledged it.
// Calling it more than once is harmless. If the connection is down, the
// acknowledgement is remembered and the server's resend after the
// reconnect is acknowledged instead.
func (m Message) Ack() {
	if m.ack != nil {
		m.ack()
	}
}

// Subscription is the messages of one subscription. For a topic they
// arrive in seq order. For a pattern they are the messages of every
// matching topic, in seq order within each topic. For a shared
// subscription they are this member's share, not in strict seq order.
type Subscription struct {
	// topic is the subscribed topic or pattern.
	topic string
	// messages is where the client puts the topic's messages; closed by the
	// engine alone.
	messages chan Message
	// stop is closed when the application unsubscribes, so a delivery
	// blocked on a full buffer gives up.
	stop chan struct{}
	// stopOnce closes stop once.
	stopOnce sync.Once
	// ended records, for the engine only, that messages was closed.
	ended bool
}

// newSubscription builds a subscription with a buffer of the given size.
func newSubscription(topic string, buffer int) *Subscription {
	return &Subscription{
		topic:    topic,
		messages: make(chan Message, buffer),
		stop:     make(chan struct{}),
	}
}

// Topic returns the subscribed topic, or the pattern for a wildcard
// subscription.
func (s *Subscription) Topic() string {
	return s.topic
}

// Messages returns the channel the topic's messages arrive on. It is
// closed when the application unsubscribes, when the server ends the
// subscription (see UnsubscribedEvent), when the topic is subscribed again
// (the new Subscription takes over), and when the client stops.
func (s *Subscription) Messages() <-chan Message {
	return s.messages
}

// halt tells the engine the application no longer wants this
// subscription's messages.
func (s *Subscription) halt() {
	s.stopOnce.Do(func() { close(s.stop) })
}

// halted reports whether the application unsubscribed.
func (s *Subscription) halted() bool {
	select {
	case <-s.stop:
		return true
	default:
		return false
	}
}

// end closes the messages channel once. Only the engine calls it.
func (s *Subscription) end() {
	if !s.ended {
		s.ended = true
		close(s.messages)
	}
}

// Event is something that happened to the connection or a subscription.
// It is one of ConnectedEvent, DisconnectedEvent, TruncatedEvent,
// StalledEvent, AuthExpiringEvent, AuthRefreshedEvent, UnsubscribedEvent
// and ServerErrorEvent.
type Event interface {
	// isEvent marks the event types.
	isEvent()
}

// ConnectedEvent reports that a connection is up and authenticated.
type ConnectedEvent struct {
	// Resumed are the topics the server restored for the durable session.
	Resumed []string
}

// DisconnectedEvent reports that the connection went down.
type DisconnectedEvent struct {
	// Code is the server's close code, or 0 when it sent none.
	Code CloseCode
	// Reconnecting says whether the client will try to reconnect.
	Reconnecting bool
}

// TruncatedEvent reports that messages on a topic were trimmed before the
// client got them.
type TruncatedEvent struct {
	// Topic is the topic.
	Topic string
	// FirstAvailable is the oldest seq still available; delivery continues
	// from here.
	FirstAvailable uint64
}

// StalledEvent reports that a QoS 1 message ran out of retries; the server
// paused the topic for this client until it acknowledges something or
// subscribes again.
type StalledEvent struct {
	// Topic is the topic.
	Topic string
	// Seq is the message that was never acknowledged.
	Seq uint64
}

// AuthExpiringEvent reports that the token will expire soon. With password
// credentials the client refreshes it; with a bare token the connection
// closes when it expires.
type AuthExpiringEvent struct {
	// ExpiresAtMs is when it expires, in milliseconds since the Unix epoch.
	ExpiresAtMs int64
}

// AuthRefreshedEvent reports that the client got a new token and the
// connection carries on.
type AuthRefreshedEvent struct{}

// UnsubscribedEvent reports that the server ended a subscription, because
// the user may no longer read the topic. The client won't resubscribe. A
// topic subscribed by name has its Subscription's channel closed; for a
// topic joined through a pattern only that topic stops, and the pattern's
// Subscription carries on.
type UnsubscribedEvent struct {
	// Topic is the topic.
	Topic string
}

// ServerErrorEvent reports an error that answers no request of the
// application, such as a failed QoS 0 publish.
type ServerErrorEvent struct {
	// Code is the error code.
	Code ErrorCode
	// Message describes it for people.
	Message string
}

// isEvent marks ConnectedEvent as an Event.
func (ConnectedEvent) isEvent() {}

// isEvent marks DisconnectedEvent as an Event.
func (DisconnectedEvent) isEvent() {}

// isEvent marks TruncatedEvent as an Event.
func (TruncatedEvent) isEvent() {}

// isEvent marks StalledEvent as an Event.
func (StalledEvent) isEvent() {}

// isEvent marks AuthExpiringEvent as an Event.
func (AuthExpiringEvent) isEvent() {}

// isEvent marks AuthRefreshedEvent as an Event.
func (AuthRefreshedEvent) isEvent() {}

// isEvent marks UnsubscribedEvent as an Event.
func (UnsubscribedEvent) isEvent() {}

// isEvent marks ServerErrorEvent as an Event.
func (ServerErrorEvent) isEvent() {}
