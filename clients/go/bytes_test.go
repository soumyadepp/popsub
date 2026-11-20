package popsub

import (
	"bytes"
	"context"
	"errors"
	"testing"
)

// binaryBody is a body that is not valid UTF-8.
var binaryBody = []byte{0x00, 0xff, 0xfe, 0x80}

// binaryBase64 is binaryBody in standard padded base64.
const binaryBase64 = "AP/+gA=="

// TestPublishBytesSendsBase64WithTheEncoding checks the frames of
// PublishBytes and PublishBytesConfirmed: the payload is standard padded
// base64, encoding is base64, and the confirmed one carries its message_id
// and qos 1 like a text publish.
func TestPublishBytesSendsBase64WithTheEncoding(t *testing.T) {
	f := newFakeServer(t)
	client := connectTest(t, f, testConfig(f))
	if err := client.PublishBytes("blobs", binaryBody); err != nil {
		t.Fatalf("publish bytes: %v", err)
	}
	published, err := client.PublishBytesConfirmed(context.Background(), "blobs", binaryBody)
	if err != nil {
		t.Fatalf("publish bytes confirmed: %v", err)
	}
	frames := f.awaitReceived(t, "publish", "blobs", 2)
	for _, frame := range frames {
		if frame["payload"] != binaryBase64 || frame["encoding"] != "base64" {
			t.Fatalf("unexpected publish frame %v", frame)
		}
	}
	if _, has := frames[0]["message_id"]; has {
		t.Fatalf("a QoS 0 publish carries no message_id: %v", frames[0])
	}
	if frames[1]["message_id"] != published.MessageID || frames[1]["qos"] != float64(1) {
		t.Fatalf("unexpected confirmed publish frame %v", frames[1])
	}
}

// TestTextPublishCarriesNoEncoding checks that Publish and
// PublishConfirmed leave the encoding field out of the frame, even for
// text that happens to be valid base64.
func TestTextPublishCarriesNoEncoding(t *testing.T) {
	f := newFakeServer(t)
	client := connectTest(t, f, testConfig(f))
	if err := client.Publish("notes", binaryBase64); err != nil {
		t.Fatalf("publish: %v", err)
	}
	if _, err := client.PublishConfirmed(context.Background(), "notes", "hello"); err != nil {
		t.Fatalf("publish confirmed: %v", err)
	}
	frames := f.awaitReceived(t, "publish", "notes", 2)
	for _, frame := range frames {
		if _, has := frame["encoding"]; has {
			t.Fatalf("a text publish must not carry an encoding: %v", frame)
		}
	}
	if frames[0]["payload"] != binaryBase64 || frames[1]["payload"] != "hello" {
		t.Fatalf("unexpected payloads %v", frames)
	}
	text, _ := encode(0, publishRequest{Type: "publish", Topic: "t", Payload: "p"})
	if string(text) != `{"type":"publish","topic":"t","payload":"p"}` {
		t.Fatalf("unexpected text frame %s", text)
	}
	encoded, _ := encode(3, publishRequest{Type: "publish", Topic: "t", Payload: "AA==", Encoding: EncodingBase64, MessageID: "m", QoS: 1})
	if string(encoded) != `{"id":3,"type":"publish","topic":"t","payload":"AA==","encoding":"base64","message_id":"m","qos":1}` {
		t.Fatalf("unexpected bytes frame %s", encoded)
	}
}

// TestQueuedBytesPublishKeepsItsEncoding checks the frames a bytes publish
// leaves for sending after a reconnect: the QoS 0 one waiting in offline
// and the confirmed one kept in unconfirmed for resending both carry the
// base64 payload, the encoding and, when confirmed, the same message_id.
func TestQueuedBytesPublishKeepsItsEncoding(t *testing.T) {
	e := newEngine(Config{})
	defer e.cancel()
	e.queuePublish(publishCommand{topic: "blobs", payload: binaryBase64, encoding: EncodingBase64})
	id := e.queuePublish(publishCommand{topic: "blobs", payload: binaryBase64, encoding: EncodingBase64, messageID: "m1", reply: make(chan publishResult, 1)})
	queued, ok := e.offline[0].(publishRequest)
	if !ok || len(e.offline) != 1 {
		t.Fatalf("expected one queued publish, got %v", e.offline)
	}
	if queued.Payload != binaryBase64 || queued.Encoding != EncodingBase64 || queued.QoS != 0 {
		t.Fatalf("unexpected queued frame %+v", queued)
	}
	resend := e.unconfirmed[id]
	if resend.Payload != binaryBase64 || resend.Encoding != EncodingBase64 || resend.MessageID != "m1" || resend.QoS != 1 {
		t.Fatalf("unexpected frame kept for resending %+v", resend)
	}
}

// TestBytesRoundTripThroughTheServer checks that bytes that are not valid
// UTF-8 come back from Message.Bytes unchanged, with the payload string
// and encoding as they travelled.
func TestBytesRoundTripThroughTheServer(t *testing.T) {
	f := newFakeServer(t)
	client := connectTest(t, f, testConfig(f))
	sub, err := client.Subscribe(context.Background(), "blobs", FromNew)
	if err != nil {
		t.Fatalf("subscribe: %v", err)
	}
	if _, err := client.PublishBytesConfirmed(context.Background(), "blobs", binaryBody); err != nil {
		t.Fatalf("publish bytes confirmed: %v", err)
	}
	message := next(t, sub)
	if message.Payload != binaryBase64 || message.Encoding != EncodingBase64 {
		t.Fatalf("unexpected message %+v", message)
	}
	body, err := message.Bytes()
	if err != nil {
		t.Fatalf("bytes: %v", err)
	}
	if !bytes.Equal(body, binaryBody) {
		t.Fatalf("expected % x, got % x", binaryBody, body)
	}
}

// TestReceivedBase64MessageDecodes checks a message frame with encoding
// base64: Bytes returns the decoded bytes, including the empty body.
func TestReceivedBase64MessageDecodes(t *testing.T) {
	f := newFakeServer(t)
	client := connectTest(t, f, testConfig(f))
	sub, err := client.Subscribe(context.Background(), "feed", FromNew)
	if err != nil {
		t.Fatalf("subscribe: %v", err)
	}
	f.inject(t, `{"type":"message","topic":"feed","seq":1,"message_id":"m1","payload":"AP/+gA==","encoding":"base64","qos":0,"published_at_ms":0}`)
	f.inject(t, `{"type":"message","topic":"feed","seq":2,"message_id":"m2","payload":"","encoding":"base64","qos":0,"published_at_ms":0}`)
	for _, want := range [][]byte{binaryBody, {}} {
		message := next(t, sub)
		if message.Encoding != EncodingBase64 {
			t.Fatalf("expected the base64 encoding, got %q", message.Encoding)
		}
		body, err := message.Bytes()
		if err != nil {
			t.Fatalf("bytes: %v", err)
		}
		if !bytes.Equal(body, want) {
			t.Fatalf("expected % x, got % x", want, body)
		}
	}
}

// TestTextMessageBytesAreItsUTF8 checks that a message with no encoding
// has the zero Encoding and that Bytes returns the payload's UTF-8, even
// when the text is valid base64.
func TestTextMessageBytesAreItsUTF8(t *testing.T) {
	f := newFakeServer(t)
	client := connectTest(t, f, testConfig(f))
	sub, err := client.Subscribe(context.Background(), "feed", FromNew)
	if err != nil {
		t.Fatalf("subscribe: %v", err)
	}
	f.inject(t, `{"type":"message","topic":"feed","seq":1,"message_id":"m1","payload":"héllo","qos":0,"published_at_ms":0}`)
	f.inject(t, `{"type":"message","topic":"feed","seq":2,"message_id":"m2","payload":"AP/+gA==","qos":0,"published_at_ms":0}`)
	for _, want := range []string{"héllo", binaryBase64} {
		message := next(t, sub)
		if message.Encoding != "" {
			t.Fatalf("expected no encoding, got %q", message.Encoding)
		}
		body, err := message.Bytes()
		if err != nil {
			t.Fatalf("bytes: %v", err)
		}
		if !bytes.Equal(body, []byte(want)) {
			t.Fatalf("expected % x, got % x", []byte(want), body)
		}
	}
}

// TestInvalidBase64PayloadIsDeliveredAndBytesFails checks that a message
// claiming base64 with a payload that does not decode still reaches the
// application with its payload string, that Bytes fails with KindProtocol,
// and that the next message arrives as usual.
func TestInvalidBase64PayloadIsDeliveredAndBytesFails(t *testing.T) {
	f := newFakeServer(t)
	client := connectTest(t, f, testConfig(f))
	sub, err := client.Subscribe(context.Background(), "feed", FromNew)
	if err != nil {
		t.Fatalf("subscribe: %v", err)
	}
	invalid := []string{"not base64!", "AP/+gA", "AP/+gB=="}
	for i, payload := range invalid {
		seq := string(rune('1' + i))
		f.inject(t, `{"type":"message","topic":"feed","seq":`+seq+`,"message_id":"m`+seq+`","payload":"`+payload+`","encoding":"base64","qos":0,"published_at_ms":0}`)
	}
	f.inject(t, `{"type":"message","topic":"feed","seq":4,"message_id":"m4","payload":"AA==","encoding":"base64","qos":0,"published_at_ms":0}`)
	for _, payload := range invalid {
		message := next(t, sub)
		if message.Payload != payload {
			t.Fatalf("expected the payload %q as it arrived, got %q", payload, message.Payload)
		}
		body, err := message.Bytes()
		if body != nil || !errors.Is(err, ErrProtocol) {
			t.Fatalf("expected a protocol error for %q, got %v and %v", payload, body, err)
		}
		var e *Error
		if !errors.As(err, &e) || e.Kind != KindProtocol {
			t.Fatalf("expected a KindProtocol *Error, got %v", err)
		}
	}
	body, err := next(t, sub).Bytes()
	if err != nil || !bytes.Equal(body, []byte{0}) {
		t.Fatalf("expected the next message's bytes, got %v and %v", body, err)
	}
}
