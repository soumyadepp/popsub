package popsub

import (
	"encoding/json"
	"errors"
	"regexp"
	"testing"
	"time"
)

// TestCeilingDoublesUpToMax checks the backoff ceiling: Initial doubled
// per attempt, never above Max, and without overflowing for huge attempts.
func TestCeilingDoublesUpToMax(t *testing.T) {
	policy := ReconnectPolicy{Initial: 250 * time.Millisecond, Max: 30 * time.Second}
	cases := map[int]time.Duration{
		0:       250 * time.Millisecond,
		1:       500 * time.Millisecond,
		3:       2 * time.Second,
		6:       16 * time.Second,
		7:       30 * time.Second,
		40:      30 * time.Second,
		1 << 30: 30 * time.Second,
		-1:      250 * time.Millisecond,
	}
	for attempt, want := range cases {
		if got := policy.Ceiling(attempt); got != want {
			t.Errorf("Ceiling(%d) = %v, want %v", attempt, got, want)
		}
	}
	huge := ReconnectPolicy{Initial: time.Hour, Max: time.Duration(1<<63 - 1)}
	if got := huge.Ceiling(30); got <= 0 {
		t.Errorf("Ceiling overflowed to %v", got)
	}
}

// TestDuplicatesAreDroppedAndReacked checks receive: below the next
// expected seq is a duplicate, re-acknowledged only when acknowledged
// before or in AckAuto.
func TestDuplicatesAreDroppedAndReacked(t *testing.T) {
	manual := newTopicState(newSubscription("t", 1), FromNew)
	if deliver, ack := manual.receive(5, 1, AckManual); !deliver || ack {
		t.Fatalf("a first QoS 1 message in manual mode: deliver=%v ack=%v", deliver, ack)
	}
	if deliver, ack := manual.receive(5, 1, AckManual); deliver || ack {
		t.Fatalf("an unacknowledged duplicate: deliver=%v ack=%v", deliver, ack)
	}
	manual.acked.insert(5)
	if deliver, ack := manual.receive(5, 1, AckManual); deliver || !ack {
		t.Fatalf("an acknowledged duplicate: deliver=%v ack=%v", deliver, ack)
	}
	if deliver, ack := manual.receive(4, 0, AckManual); deliver || ack {
		t.Fatalf("a QoS 0 duplicate: deliver=%v ack=%v", deliver, ack)
	}
	auto := newTopicState(newSubscription("t", 1), FromNew)
	if deliver, ack := auto.receive(1, 1, AckAuto); !deliver || !ack {
		t.Fatalf("a first QoS 1 message in auto mode: deliver=%v ack=%v", deliver, ack)
	}
	if deliver, ack := auto.receive(1, 1, AckAuto); deliver || !ack {
		t.Fatalf("a duplicate in auto mode: deliver=%v ack=%v", deliver, ack)
	}
}

// TestAdvanceAndResumeFrom checks that replay_from and first_available
// only move the next expected seq forward, and that a resubscription
// starts there.
func TestAdvanceAndResumeFrom(t *testing.T) {
	topic := newTopicState(newSubscription("t", 1), FromLast(3))
	if got := topic.resumeFrom(); got != FromLast(3) {
		t.Fatalf("before anything arrived, expected the original start, got %v", got)
	}
	topic.advance(10)
	topic.advance(4)
	if got := topic.resumeFrom(); got != FromSeq(10) {
		t.Fatalf("expected seq 10, got %v", got)
	}
	topic.receive(12, 0, AckAuto)
	if got := topic.resumeFrom(); got != FromSeq(13) {
		t.Fatalf("expected seq 13, got %v", got)
	}
}

// TestAckMemoryForgetsTheOldest checks that the memory keeps only the
// newest seqs up to its limit.
func TestAckMemoryForgetsTheOldest(t *testing.T) {
	memory := newAckMemory(3)
	for seq := uint64(1); seq <= 4; seq++ {
		memory.insert(seq)
	}
	memory.insert(4)
	if memory.contains(1) || !memory.contains(2) || !memory.contains(4) {
		t.Fatalf("unexpected memory %v", memory.order)
	}
}

// TestStartFromJSON checks every start point's wire form, both ways, and
// that invalid ones are refused.
func TestStartFromJSON(t *testing.T) {
	cases := map[string]StartFrom{
		`"resume"`:            FromResume,
		`"new"`:               FromNew,
		`{"last":10}`:         FromLast(10),
		`{"since_ms":179480}`: FromSinceMs(179480),
		`{"seq":42}`:          FromSeq(42),
	}
	for wire, from := range cases {
		encoded, err := json.Marshal(from)
		if err != nil || string(encoded) != wire {
			t.Errorf("Marshal(%v) = %s, %v; want %s", from, encoded, err, wire)
		}
		var decoded StartFrom
		if err := json.Unmarshal([]byte(wire), &decoded); err != nil || decoded != from {
			t.Errorf("Unmarshal(%s) = %v, %v", wire, decoded, err)
		}
	}
	for _, wire := range []string{`"later"`, `{"seq":1,"last":2}`, `{"other":1}`, `{}`, `7`} {
		var decoded StartFrom
		if err := json.Unmarshal([]byte(wire), &decoded); err == nil {
			t.Errorf("Unmarshal(%s) should fail", wire)
		}
	}
}

// TestEncodePutsTheIDFirst checks request encoding with and without an id.
func TestEncodePutsTheIDFirst(t *testing.T) {
	withID, _ := encode(7, unsubscribeRequest{Type: "unsubscribe", Topic: "t"})
	if string(withID) != `{"id":7,"type":"unsubscribe","topic":"t"}` {
		t.Fatalf("unexpected frame %s", withID)
	}
	withoutID, _ := encode(0, ackRequest{Type: "ack", Topic: "t", Seq: 3})
	if string(withoutID) != `{"type":"ack","topic":"t","seq":3}` {
		t.Fatalf("unexpected frame %s", withoutID)
	}
	enabled, _ := encode(1, setEnabledRequest{Type: "set_enabled", Username: "u", Enabled: false})
	if string(enabled) != `{"id":1,"type":"set_enabled","username":"u","enabled":false}` {
		t.Fatalf("enabled=false must be sent: %s", enabled)
	}
}

// TestShouldReconnectByCloseCode checks the reconnect rules, including
// token_expired depending on the credentials.
func TestShouldReconnectByCloseCode(t *testing.T) {
	password := &engine{cfg: Config{Credentials: PasswordCredentials("u", "p")}}
	token := &engine{cfg: Config{Credentials: TokenCredentials("t")}}
	for _, code := range []CloseCode{CloseSessionTakenOver, CloseUnsupportedProtocol, CloseAuthFailed, CloseAccountDisabled} {
		if password.shouldReconnect(code) {
			t.Errorf("should not reconnect after %d", code)
		}
	}
	for _, code := range []CloseCode{0, CloseGoingAway, CloseIdleTimeout, CloseSlowConsumer, CloseTryAgainLater} {
		if !password.shouldReconnect(code) {
			t.Errorf("should reconnect after %d", code)
		}
	}
	if !password.shouldReconnect(CloseTokenExpired) || token.shouldReconnect(CloseTokenExpired) {
		t.Error("token_expired reconnects only with password credentials")
	}
	if knownCloseCode(1000) != 0 || knownCloseCode(4003) != CloseSessionTakenOver {
		t.Error("unexpected close code mapping")
	}
}

// TestErrorsMatchTheirKind checks errors.Is against the sentinels and the
// wrapped cause, and Temporary.
func TestErrorsMatchTheirKind(t *testing.T) {
	closed := closedError(CloseSessionTakenOver)
	if !errors.Is(closed, ErrClosed) || errors.Is(closed, ErrServer) {
		t.Fatal("a closed error should match ErrClosed only")
	}
	cause := errors.New("connection refused")
	connect := connectError("dialing failed", cause)
	if !errors.Is(connect, ErrConnect) || !errors.Is(connect, cause) || !connect.Temporary() {
		t.Fatal("a connect error should match ErrConnect and its cause, and be temporary")
	}
	limited := serverError(&serverFrame{Code: "rate_limited", RetryAfter: 3})
	if limited.RetryAfter != 3*time.Second || !limited.Temporary() {
		t.Fatalf("unexpected rate limit error %+v", limited)
	}
	if serverError(&serverFrame{Code: "not_authorized"}).Temporary() {
		t.Fatal("not_authorized is not temporary")
	}
}

// TestNewUUIDIsVersion4 checks the message id format.
func TestNewUUIDIsVersion4(t *testing.T) {
	pattern := regexp.MustCompile(`^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$`)
	first, second := newUUID(), newUUID()
	if !pattern.MatchString(first) || first == second {
		t.Fatalf("unexpected UUIDs %s, %s", first, second)
	}
}

// TestReconnectPolicyLimits checks when a policy gives up: never for 0,
// after the count for a positive MaxAttempts, after the first failure for
// NoRetries, and that other negative values are refused.
func TestReconnectPolicyLimits(t *testing.T) {
	forever := ReconnectPolicy{}
	if !forever.retries() || forever.exhausted(1) || forever.exhausted(1<<30) {
		t.Error("MaxAttempts 0 should keep trying")
	}
	three := ReconnectPolicy{MaxAttempts: 3}
	if three.exhausted(3) || !three.exhausted(4) {
		t.Error("MaxAttempts 3 should give up on the fourth failure")
	}
	off := ReconnectPolicy{MaxAttempts: NoRetries}
	if off.retries() || off.exhausted(0) || !off.exhausted(1) {
		t.Error("NoRetries should give up after the first failure")
	}
	base := Config{URL: "ws://127.0.0.1:1", Credentials: TokenCredentials("t")}
	base.Reconnect.MaxAttempts = NoRetries
	if _, err := base.withDefaults(); err != nil {
		t.Errorf("NoRetries should be accepted: %v", err)
	}
	base.Reconnect.MaxAttempts = -2
	if _, err := base.withDefaults(); !errors.Is(err, ErrConfig) {
		t.Errorf("MaxAttempts -2 should be a config error, got %v", err)
	}
}
