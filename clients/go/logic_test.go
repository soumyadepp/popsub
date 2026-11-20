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

// TestMatchesPattern checks the pattern matcher: + is one whole level, a
// trailing # is the rest (including nothing), and plain names match only
// themselves.
func TestMatchesPattern(t *testing.T) {
	cases := []struct {
		pattern string
		topic   string
		want    bool
	}{
		{"orders", "orders", true},
		{"orders", "orders/eu", false},
		{"#", "anything/at/all", true},
		{"orders/#", "orders/eu", true},
		{"orders/#", "orders/eu/123", true},
		{"orders/#", "orders", true},
		{"orders/#", "ordersx/eu", false},
		{"orders/#", "audit", false},
		{"sensors/+/temp", "sensors/kitchen/temp", true},
		{"sensors/+/temp", "sensors/kitchen/humidity", false},
		{"sensors/+/temp", "sensors/temp", false},
		{"sensors/+/temp", "sensors/a/b/temp", false},
		{"sensors/+", "sensors/kitchen", true},
		{"sensors/+", "sensors", false},
		{"sensors/+", "sensors/kitchen/temp", false},
		{"+/+", "a/b", true},
		{"+", "a/b", false},
		{"+/#", "a", true},
		{"a/+/#", "a/b/c/d", true},
		{"a/+/#", "a", false},
	}
	for _, c := range cases {
		if got := matchesPattern(c.pattern, c.topic); got != c.want {
			t.Errorf("matchesPattern(%q, %q) = %v, want %v", c.pattern, c.topic, got, c.want)
		}
	}
	if !isPattern("orders/#") || !isPattern("a/+/b") || isPattern("orders/eu") {
		t.Error("isPattern should be true exactly for names with + or #")
	}
}

// TestSharedSubscriptionDeduplicatesBySeenSet checks that a shared
// subscription accepts seqs in any order, drops repeats with the usual
// re-acknowledge rule, ignores replay_from and truncated, and rejoins
// with resume.
func TestSharedSubscriptionDeduplicatesBySeenSet(t *testing.T) {
	shared := newTopicState(newSubscription("jobs", 1), FromNew)
	shared.group = "workers"
	for _, seq := range []uint64{5, 2, 9} {
		if deliver, ack := shared.receive(seq, 1, AckManual); !deliver || ack {
			t.Fatalf("seq %d out of order should be handed over unacknowledged: deliver=%v ack=%v", seq, deliver, ack)
		}
	}
	if deliver, ack := shared.receive(2, 1, AckManual); deliver || ack {
		t.Fatalf("a repeat that was never acknowledged: deliver=%v ack=%v", deliver, ack)
	}
	shared.acked.insert(2)
	if deliver, ack := shared.receive(2, 1, AckManual); deliver || !ack {
		t.Fatalf("a repeat that was acknowledged: deliver=%v ack=%v", deliver, ack)
	}
	shared.advance(100)
	if deliver, _ := shared.receive(7, 1, AckManual); !deliver {
		t.Fatal("replay_from must not make a shared subscription drop lower seqs")
	}
	if got := shared.resumeFrom(); got != FromResume {
		t.Fatalf("a shared subscription rejoins with resume, got %v", got)
	}
	auto := newTopicState(newSubscription("jobs", 1), FromNew)
	auto.group = "workers"
	auto.receive(3, 1, AckAuto)
	if deliver, ack := auto.receive(3, 1, AckAuto); deliver || !ack {
		t.Fatalf("a repeat in auto mode: deliver=%v ack=%v", deliver, ack)
	}
}

// TestSeenSetIsBounded checks that a shared subscription forgets the
// oldest delivered seqs beyond its limit, so the set can't grow forever.
func TestSeenSetIsBounded(t *testing.T) {
	shared := newTopicState(newSubscription("jobs", 1), FromNew)
	shared.group = "workers"
	for seq := uint64(1); seq <= ackMemorySize+10; seq++ {
		shared.receive(seq, 0, AckAuto)
	}
	if len(shared.seen.order) != ackMemorySize || shared.seen.contains(10) || !shared.seen.contains(11) {
		t.Fatalf("expected the oldest 10 forgotten, holding %d", len(shared.seen.order))
	}
}

// TestPatternsOwnTheTopicsTheyMatch checks adopt and release: the oldest
// matching pattern owns a topic, an owned topic tracks its own seqs, the
// server ending one topic leaves the pattern's subscription open, and
// releasing a pattern releases its topics and closes its channel.
func TestPatternsOwnTheTopicsTheyMatch(t *testing.T) {
	e := newEngine(Config{})
	defer e.cancel()
	older := newSubscription("orders/#", 1)
	newer := newSubscription("orders/+", 1)
	byName := newSubscription("audit", 1)
	e.addTopic("orders/#", FromNew, "", older)
	e.addTopic("orders/+", FromNew, "", newer)
	e.addTopic("audit", FromNew, "", byName)
	if !e.adopt("orders/a", 4, true) || !e.adopt("orders/b", 0, false) || e.adopt("elsewhere", 1, true) {
		t.Fatal("adopt should match orders/a and orders/b only")
	}
	a, b := e.topics["orders/a"], e.topics["orders/b"]
	if a.owner != "orders/#" || a.sub != older || a.resumeFrom() != FromSeq(4) {
		t.Fatalf("orders/a should belong to the older pattern from seq 4: %+v", a)
	}
	if b.owner != "orders/#" || b.hasNext || b.resumeFrom() != FromSeq(1) {
		t.Fatalf("orders/b adopted from a message should start unknown: %+v", b)
	}
	if deliver, _ := a.receive(3, 0, AckAuto); deliver {
		t.Fatal("a seq below the adopted topic's start is a duplicate")
	}
	if !e.endTopic("orders/a") || older.ended {
		t.Fatal("ending one owned topic must leave the pattern's subscription open")
	}
	if got := e.release("orders/#", newer); got != nil {
		t.Fatalf("release with another subscription should do nothing, got %v", got)
	}
	e.adopt("orders/c", 1, true)
	names := e.release("orders/#", nil)
	if len(names) != 3 || names[0] != "orders/#" || names[1] != "orders/b" || names[2] != "orders/c" {
		t.Fatalf("unexpected released names %v", names)
	}
	if !older.ended || newer.ended || len(e.topics) != 1 {
		t.Fatalf("releasing the pattern should close only its subscription and drop its topics: %v", e.topics)
	}
	if got := e.release("audit", nil); len(got) != 1 || !byName.ended {
		t.Fatalf("releasing a topic by name should close its subscription, got %v", got)
	}
}

// TestTopicFieldReadsNamesAndObjects checks that a frame's topic field
// parses both as a name and as the topic_info object.
func TestTopicFieldReadsNamesAndObjects(t *testing.T) {
	var named, described serverFrame
	if err := json.Unmarshal([]byte(`{"type":"message","topic":"orders","seq":2}`), &named); err != nil || named.Topic.name != "orders" {
		t.Fatalf("unexpected %+v, %v", named, err)
	}
	wire := `{"id":3,"type":"topic_info","topic":{"name":"orders","first_seq":4,"last_seq":3,"messages":0,"subscribers":2}}`
	if err := json.Unmarshal([]byte(wire), &described); err != nil || described.Topic.name != "" {
		t.Fatalf("unexpected %+v, %v", described, err)
	}
	var info TopicInfo
	if err := json.Unmarshal(described.Topic.raw, &info); err != nil || info != (TopicInfo{Name: "orders", FirstSeq: 4, LastSeq: 3, Subscribers: 2}) {
		t.Fatalf("unexpected topic %+v, %v", info, err)
	}
}

// TestUnclaimedTopicsAreClaimedBySubscribing checks the claim bookkeeping:
// resumed topics the engine is not tracking are unclaimed, subscribing by
// name or by a matching pattern tracks them as claiming, a claiming topic
// asks again on a new connection the way it was first asked, and its
// subscribed ends the claim and sets the next expected seq.
func TestUnclaimedTopicsAreClaimedBySubscribing(t *testing.T) {
	e := newEngine(Config{})
	defer e.cancel()
	held := newSubscription("held", 1)
	e.addTopic("held", FromNew, "", held)
	e.markUnclaimed([]string{"held", "jobs", "tasks/b", "tasks/a", "audit"})
	if len(e.unclaimed) != 4 || e.unclaimed["held"] {
		t.Fatalf("expected the four untracked topics unclaimed, got %v", e.unclaimed)
	}
	jobs := newSubscription("jobs", 1)
	if claimed := e.addTopic("jobs", FromLast(2), "", jobs); claimed != nil {
		t.Fatalf("a topic by name claims nothing else, got %v", claimed)
	}
	tasks := newSubscription("tasks/#", 1)
	claimed := e.addTopic("tasks/#", FromNew, "", tasks)
	if len(claimed) != 2 || claimed[0] != "tasks/a" || claimed[1] != "tasks/b" {
		t.Fatalf("expected the pattern to claim tasks/a and tasks/b in order, got %v", claimed)
	}
	if len(e.unclaimed) != 1 || !e.unclaimed["audit"] {
		t.Fatalf("expected only audit left unclaimed, got %v", e.unclaimed)
	}
	byName, owned := e.topics["jobs"], e.topics["tasks/a"]
	if !byName.claiming || byName.restoreFrom() != FromLast(2) {
		t.Fatalf("jobs should be claiming and ask again from last 2: %+v", byName)
	}
	if !owned.claiming || owned.owner != "tasks/#" || owned.sub != tasks || owned.restoreFrom() != FromResume {
		t.Fatalf("tasks/a should be claiming for the pattern and ask again from resume: %+v", owned)
	}
	if e.topics["held"].claiming {
		t.Fatal("a topic tracked before the connection is not claiming")
	}
	again := newSubscription("jobs", 1)
	e.addTopic("jobs", FromNew, "", again)
	byName = e.topics["jobs"]
	if !byName.claiming || !jobs.ended {
		t.Fatalf("subscribing again before subscribed should keep the claim: %+v", byName)
	}
	byName.advance(9)
	byName.claim()
	byName.advance(4)
	if byName.claiming || byName.restoreFrom() != FromSeq(4) {
		t.Fatalf("subscribed should end the claim and start from replay_from: %+v", byName)
	}
	byName.claim()
	if byName.restoreFrom() != FromSeq(4) {
		t.Fatalf("claim on a claimed topic should change nothing: %+v", byName)
	}
	e.markUnclaimed([]string{"jobs", "audit"})
	if len(e.unclaimed) != 1 || !e.unclaimed["audit"] {
		t.Fatalf("a new connection should leave only audit unclaimed, got %v", e.unclaimed)
	}
}

// TestHeldLeavesNothingUnclaimed checks that when the server holds the
// session's topics no resumed topic is unclaimed, so a subscribe by name
// is not a claim and a pattern claims nothing by name, and that a later
// connection the server does not hold goes back to marking them.
func TestHeldLeavesNothingUnclaimed(t *testing.T) {
	e := newEngine(Config{})
	defer e.cancel()
	e.markUnclaimed([]string{"stale"})
	e.held = true
	e.markUnclaimed([]string{"jobs", "tasks/a"})
	if len(e.unclaimed) != 0 {
		t.Fatalf("expected nothing unclaimed while held, got %v", e.unclaimed)
	}
	e.addTopic("jobs", FromResume, "", newSubscription("jobs", 1))
	if e.topics["jobs"].claiming {
		t.Fatal("a subscribe to a held topic is not a claim")
	}
	if claimed := e.addTopic("tasks/#", FromNew, "", newSubscription("tasks/#", 1)); claimed != nil {
		t.Fatalf("a pattern claims nothing by name while held, got %v", claimed)
	}
	e.held = false
	e.markUnclaimed([]string{"jobs", "tasks/a"})
	if len(e.unclaimed) != 1 || !e.unclaimed["tasks/a"] {
		t.Fatalf("expected tasks/a unclaimed once the server stops holding, got %v", e.unclaimed)
	}
}

// TestReplacingAPatternMovesItsTopics checks that subscribing again to a
// held pattern closes the earlier subscription and moves the topics it
// owned to the new one with their next expected seq, leaving topics owned
// by other patterns or subscribed by name alone.
func TestReplacingAPatternMovesItsTopics(t *testing.T) {
	e := newEngine(Config{})
	defer e.cancel()
	first := newSubscription("orders/#", 1)
	other := newSubscription("audit/#", 1)
	e.addTopic("orders/#", FromNew, "", first)
	e.addTopic("audit/#", FromNew, "", other)
	e.adopt("orders/a", 1, true)
	e.adopt("audit/a", 1, true)
	e.topics["orders/a"].receive(1, 1, AckAuto)
	second := newSubscription("orders/#", 1)
	e.addTopic("orders/#", FromNew, "", second)
	moved := e.topics["orders/a"]
	if !first.ended || second.ended || other.ended {
		t.Fatal("only the replaced subscription should be closed")
	}
	if moved.sub != second || moved.owner != "orders/#" || moved.resumeFrom() != FromSeq(2) {
		t.Fatalf("orders/a should move to the new subscription expecting seq 2: %+v", moved)
	}
	if deliver, ack := moved.receive(1, 1, AckAuto); deliver || !ack {
		t.Fatal("a repeat of seq 1 should be acknowledged again and not handed over")
	}
	if e.topics["audit/a"].sub != other || e.patterns["orders/#"].sub != second {
		t.Fatal("the other pattern's topic should stay with its subscription")
	}
	e.adopt("orders/b", 1, true)
	if e.topics["orders/b"].sub != second {
		t.Fatal("a topic joined after the replacement should belong to the new subscription")
	}
}
