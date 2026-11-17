package popsub

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/coder/websocket"
	"go.uber.org/goleak"
)

// TestMain fails the package's tests if any goroutine outlives them.
func TestMain(m *testing.M) {
	goleak.VerifyTestMain(m)
}

// testConfig is a configuration for the fake server with fast reconnects.
func testConfig(f *fakeServer) Config {
	return Config{
		URL:            f.url(),
		Credentials:    PasswordCredentials("alice", "password"),
		Reconnect:      ReconnectPolicy{Initial: 10 * time.Millisecond, Max: 50 * time.Millisecond},
		RequestTimeout: 2 * time.Second,
	}
}

// connectTest connects to the fake server and closes the client when the
// test ends.
func connectTest(t *testing.T, f *fakeServer, cfg Config) *Client {
	t.Helper()
	client, err := Connect(context.Background(), cfg)
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	t.Cleanup(func() { client.Close() })
	return client
}

// next waits for the next message on a subscription.
func next(t *testing.T, sub *Subscription) Message {
	t.Helper()
	select {
	case message, ok := <-sub.Messages():
		if !ok {
			t.Fatal("the subscription ended")
		}
		return message
	case <-time.After(2 * time.Second):
		t.Fatal("no message arrived")
	}
	return Message{}
}

// TestCloseStopsEveryGoroutine checks that Close stops every goroutine the
// client started, after it connected, subscribed and received.
func TestCloseStopsEveryGoroutine(t *testing.T) {
	f := newFakeServer(t)
	client, err := Connect(context.Background(), testConfig(f))
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	sub, err := client.Subscribe(context.Background(), "orders", FromNew)
	if err != nil {
		t.Fatalf("subscribe: %v", err)
	}
	if _, err := client.PublishConfirmed(context.Background(), "orders", "hello"); err != nil {
		t.Fatalf("publish: %v", err)
	}
	next(t, sub)
	if err := client.Close(); err != nil {
		t.Fatalf("close: %v", err)
	}
	if _, ok := <-sub.Messages(); ok {
		t.Fatal("the subscription's channel is still open after Close")
	}
	f.server.Close()
	goleak.VerifyNone(t)
}

// TestConnectRejectsBadCredentials checks that a rejected login fails
// Connect at once with the server's code, inspectable with errors.Is and
// errors.As.
func TestConnectRejectsBadCredentials(t *testing.T) {
	f := newFakeServer(t)
	cfg := testConfig(f)
	cfg.Credentials = PasswordCredentials("alice", "wrong")
	_, err := Connect(context.Background(), cfg)
	var e *Error
	if !errors.As(err, &e) || e.Code != CodeInvalidCredentials || !errors.Is(err, ErrServer) {
		t.Fatalf("expected invalid_credentials, got %v", err)
	}
}

// TestConnectFailsForInvalidConfig checks that a bad URL is a config error.
func TestConnectFailsForInvalidConfig(t *testing.T) {
	_, err := Connect(context.Background(), Config{URL: "http://example.com", Credentials: TokenCredentials("t")})
	if !errors.Is(err, ErrConfig) {
		t.Fatalf("expected a config error, got %v", err)
	}
}

// TestConnectGivesUpAfterMaxAttempts checks that an unreachable server
// fails Connect with a connect error once the attempts run out.
func TestConnectGivesUpAfterMaxAttempts(t *testing.T) {
	f := newFakeServer(t)
	cfg := testConfig(f)
	f.server.Close()
	cfg.Reconnect.MaxAttempts = 2
	_, err := Connect(context.Background(), cfg)
	if !errors.Is(err, ErrConnect) {
		t.Fatalf("expected a connect error, got %v", err)
	}
}

// TestSubscribeHonoursContext checks that a subscribe the server never
// answers ends when its context does, with an error matching both
// ErrTimeout and context.DeadlineExceeded.
func TestSubscribeHonoursContext(t *testing.T) {
	f := newFakeServer(t)
	client := connectTest(t, f, testConfig(f))
	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()
	_, err := client.Subscribe(ctx, "hang", FromNew)
	if !errors.Is(err, ErrTimeout) || !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("expected a timeout wrapping the context error, got %v", err)
	}
}

// TestDuplicatesReachTheApplicationOnce checks that a redelivered seq is
// dropped, with a buffer of one so delivery waits on the application.
func TestDuplicatesReachTheApplicationOnce(t *testing.T) {
	f := newFakeServer(t)
	cfg := testConfig(f)
	cfg.SubscriptionBuffer = 1
	client := connectTest(t, f, cfg)
	sub, err := client.Subscribe(context.Background(), "feed", FromNew)
	if err != nil {
		t.Fatalf("subscribe: %v", err)
	}
	for _, seq := range []string{"1", "1", "2", "1", "3"} {
		f.inject(t, `{"type":"message","topic":"feed","seq":`+seq+`,"message_id":"m`+seq+`","payload":"p","qos":1,"published_at_ms":0}`)
	}
	for _, want := range []uint64{1, 2, 3} {
		if got := next(t, sub).Seq; got != want {
			t.Fatalf("expected seq %d, got %d", want, got)
		}
	}
	select {
	case message := <-sub.Messages():
		t.Fatalf("unexpected extra message %d", message.Seq)
	case <-time.After(100 * time.Millisecond):
	}
}

// TestBadServerInputIsIgnored checks that malformed and unexpected frames
// neither panic nor break the connection, and that an unknown error code
// is reported as internal.
func TestBadServerInputIsIgnored(t *testing.T) {
	f := newFakeServer(t)
	client := connectTest(t, f, testConfig(f))
	for _, text := range []string{
		`not json`,
		`[1, 2, 3]`,
		`{"type":"message","topic":"feed","seq":"one"}`,
		`{"type":"message","topic":"nobody","seq":7,"qos":1}`,
		`{"id":"text-id","type":"published","seq":3}`,
		`{"id":424242,"type":"user","user":"not an object"}`,
		`{"type":"subscribed"}`,
		`{"type":"unsubscribed","topic":"never-subscribed"}`,
		`{"type":"something_new","field":true}`,
		`{"type":"error","code":"brand_new_code","message":"m"}`,
	} {
		f.inject(t, text)
	}
	deadline := time.After(2 * time.Second)
	for {
		select {
		case event := <-client.Events():
			if e, ok := event.(ServerErrorEvent); ok {
				if e.Code != CodeInternal {
					t.Fatalf("expected an unknown code to read as internal, got %s", e.Code)
				}
				if _, err := client.Admin().ListUsers(context.Background()); err != nil {
					t.Fatalf("the connection is unusable after bad input: %v", err)
				}
				return
			}
		case <-deadline:
			t.Fatal("no server_error event for the unknown code")
		}
	}
}

// TestReconnectResubscribesFromNextExpected checks that after the server
// drops the connection the client reconnects and asks for the next seq it
// expects, so a message published meanwhile still arrives once.
func TestReconnectResubscribesFromNextExpected(t *testing.T) {
	f := newFakeServer(t)
	client := connectTest(t, f, testConfig(f))
	<-f.authenticated
	sub, err := client.Subscribe(context.Background(), "feed", FromNew)
	if err != nil {
		t.Fatalf("subscribe: %v", err)
	}
	if _, err := client.PublishConfirmed(context.Background(), "feed", "one"); err != nil {
		t.Fatalf("publish: %v", err)
	}
	next(t, sub)
	f.dropConnection(websocket.StatusGoingAway)
	<-f.authenticated
	if _, err := client.PublishConfirmed(context.Background(), "feed", "two"); err != nil {
		t.Fatalf("publish after reconnecting: %v", err)
	}
	if got := next(t, sub).Seq; got != 2 {
		t.Fatalf("expected seq 2 after reconnecting, got %d", got)
	}
}

// TestCallsAfterCloseFail checks that every call on a closed client fails
// with ErrClosed.
func TestCallsAfterCloseFail(t *testing.T) {
	f := newFakeServer(t)
	client, err := Connect(context.Background(), testConfig(f))
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	client.Close()
	if err := client.Publish("t", "x"); !errors.Is(err, ErrClosed) {
		t.Fatalf("publish: expected ErrClosed, got %v", err)
	}
	if _, err := client.PublishConfirmed(context.Background(), "t", "x"); !errors.Is(err, ErrClosed) {
		t.Fatalf("publish_confirmed: expected ErrClosed, got %v", err)
	}
	if _, err := client.Admin().ListUsers(context.Background()); !errors.Is(err, ErrClosed) {
		t.Fatalf("list_users: expected ErrClosed, got %v", err)
	}
	if !errors.Is(client.Err(), ErrClosed) {
		t.Fatalf("Err: expected ErrClosed, got %v", client.Err())
	}
}

// TestNoRetriesFailsConnectAfterOneAttempt checks that with reconnecting
// off, a server that refuses the WebSocket handshake fails Connect after
// exactly one attempt, at once, even with a backoff long enough that a
// retry would time the call out.
func TestNoRetriesFailsConnectAfterOneAttempt(t *testing.T) {
	var attempts atomic.Int32
	refusing := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		attempts.Add(1)
		http.Error(w, "unavailable", http.StatusServiceUnavailable)
	}))
	defer refusing.Close()
	cfg := Config{
		URL:            "ws" + strings.TrimPrefix(refusing.URL, "http"),
		Credentials:    PasswordCredentials("alice", "password"),
		Reconnect:      ReconnectPolicy{Initial: time.Hour, Max: time.Hour, MaxAttempts: NoRetries},
		RequestTimeout: 5 * time.Second,
	}
	started := time.Now()
	_, err := Connect(context.Background(), cfg)
	if !errors.Is(err, ErrConnect) {
		t.Fatalf("expected a connect error, got %v", err)
	}
	if got := attempts.Load(); got != 1 {
		t.Fatalf("expected one attempt, got %d", got)
	}
	if elapsed := time.Since(started); elapsed > 2*time.Second {
		t.Fatalf("Connect took %v; it should fail at once", elapsed)
	}
}

// TestNoRetriesFailsConnectToAClosedServer checks the same against an
// address nothing listens on.
func TestNoRetriesFailsConnectToAClosedServer(t *testing.T) {
	f := newFakeServer(t)
	cfg := testConfig(f)
	f.server.Close()
	cfg.Reconnect = ReconnectPolicy{Initial: time.Hour, Max: time.Hour, MaxAttempts: NoRetries}
	started := time.Now()
	_, err := Connect(context.Background(), cfg)
	if !errors.Is(err, ErrConnect) || time.Since(started) > time.Second {
		t.Fatalf("expected a prompt connect error, got %v after %v", err, time.Since(started))
	}
}

// TestNoRetriesStopsTheClientWhenTheConnectionDrops checks that with
// reconnecting off, a dropped connection is reported as not reconnecting
// and stops the client with the close code that ended it.
func TestNoRetriesStopsTheClientWhenTheConnectionDrops(t *testing.T) {
	f := newFakeServer(t)
	cfg := testConfig(f)
	cfg.Reconnect.MaxAttempts = NoRetries
	client := connectTest(t, f, cfg)
	<-f.authenticated
	f.dropConnection(websocket.StatusGoingAway)
	deadline := time.After(2 * time.Second)
	for {
		select {
		case event := <-client.Events():
			down, ok := event.(DisconnectedEvent)
			if !ok {
				continue
			}
			if down.Reconnecting || down.Code != CloseGoingAway {
				t.Fatalf("unexpected %+v", down)
			}
			<-client.Done()
			var e *Error
			if !errors.As(client.Err(), &e) || e.Kind != KindClosed || e.CloseCode != CloseGoingAway {
				t.Fatalf("expected closed with 1001, got %v", client.Err())
			}
			return
		case <-deadline:
			t.Fatal("no disconnected event")
		}
	}
}

// TestWildcardSubscriptionCarriesEveryMatchingTopic checks a pattern end
// to end: topics the server announces are tracked separately, their
// messages arrive on the one subscription under their own names, the
// server ending one topic stops only that topic, and a topic matching no
// pattern is dropped.
func TestWildcardSubscriptionCarriesEveryMatchingTopic(t *testing.T) {
	f := newFakeServer(t)
	client := connectTest(t, f, testConfig(f))
	sub, err := client.Subscribe(context.Background(), "orders/#", FromNew)
	if err != nil {
		t.Fatalf("subscribe: %v", err)
	}
	message := func(topic, seq string) string {
		return `{"type":"message","topic":"` + topic + `","seq":` + seq + `,"message_id":"m","payload":"p","qos":0,"published_at_ms":0}`
	}
	f.inject(t, `{"type":"subscribed","topic":"orders/a","replay_from":3,"replay_to":2}`)
	f.inject(t, message("orders/a", "2"))
	f.inject(t, message("orders/a", "3"))
	f.inject(t, message("elsewhere", "1"))
	f.inject(t, message("orders/b", "1"))
	f.inject(t, message("orders/b", "1"))
	f.inject(t, `{"type":"unsubscribed","topic":"orders/a"}`)
	f.inject(t, message("orders/b", "2"))
	want := []Message{{Topic: "orders/a", Seq: 3}, {Topic: "orders/b", Seq: 1}, {Topic: "orders/b", Seq: 2}}
	for _, expected := range want {
		got := next(t, sub)
		if got.Topic != expected.Topic || got.Seq != expected.Seq {
			t.Fatalf("expected %s #%d, got %s #%d", expected.Topic, expected.Seq, got.Topic, got.Seq)
		}
	}
	if err := client.Unsubscribe(context.Background(), "orders/#"); err != nil {
		t.Fatalf("unsubscribe: %v", err)
	}
	if _, ok := <-sub.Messages(); ok {
		t.Fatal("the pattern's channel should be closed after unsubscribing")
	}
}

// TestSharedSubscriptionAcceptsOutOfOrderSeqs checks a shared subscription
// end to end: seqs arrive in any order and a repeat is dropped.
func TestSharedSubscriptionAcceptsOutOfOrderSeqs(t *testing.T) {
	f := newFakeServer(t)
	client := connectTest(t, f, testConfig(f))
	sub, err := client.SubscribeShared(context.Background(), "jobs", "workers", FromNew)
	if err != nil {
		t.Fatalf("subscribe: %v", err)
	}
	for _, seq := range []string{"4", "2", "4", "6"} {
		f.inject(t, `{"type":"message","topic":"jobs","seq":`+seq+`,"message_id":"m","payload":"p","qos":1,"published_at_ms":0}`)
	}
	for _, want := range []uint64{4, 2, 6} {
		if got := next(t, sub).Seq; got != want {
			t.Fatalf("expected seq %d, got %d", want, got)
		}
	}
	if _, err := client.SubscribeShared(context.Background(), "jobs", "", FromNew); !errors.Is(err, ErrConfig) {
		t.Fatalf("an empty group should be a config error, got %v", err)
	}
}
