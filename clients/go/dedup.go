package popsub

// ackMemorySize is how many acknowledged seqs a subscription remembers, to
// acknowledge again duplicates the application already acknowledged.
const ackMemorySize = 4096

// ackMemory is the seqs a subscription acknowledged recently, forgetting
// the oldest beyond a limit.
type ackMemory struct {
	// limit is how many seqs it keeps.
	limit int
	// seen holds the acknowledged seqs.
	seen map[uint64]struct{}
	// order holds the same seqs, oldest first.
	order []uint64
}

// newAckMemory builds an empty memory keeping up to limit seqs.
func newAckMemory(limit int) *ackMemory {
	return &ackMemory{limit: limit, seen: make(map[uint64]struct{})}
}

// insert remembers an acknowledged seq, forgetting the oldest beyond the limit.
func (a *ackMemory) insert(seq uint64) {
	if _, ok := a.seen[seq]; ok {
		return
	}
	a.seen[seq] = struct{}{}
	a.order = append(a.order, seq)
	if len(a.order) > a.limit {
		delete(a.seen, a.order[0])
		a.order = a.order[1:]
	}
}

// contains reports whether a seq is remembered as acknowledged.
func (a *ackMemory) contains(seq uint64) bool {
	_, ok := a.seen[seq]
	return ok
}

// topicState is the engine's record of one subscribed topic.
type topicState struct {
	// sub is the application's subscription.
	sub *Subscription
	// from is where replay started when the application subscribed.
	from StartFrom
	// next is the next seq the application should see, when hasNext.
	next uint64
	// hasNext says whether next is known yet.
	hasNext bool
	// acked is what it has acknowledged.
	acked *ackMemory
}

// newTopicState builds the record of a new subscription.
func newTopicState(sub *Subscription, from StartFrom) *topicState {
	return &topicState{sub: sub, from: from, acked: newAckMemory(ackMemorySize)}
}

// advance moves the next expected seq forward to at least seq, for
// subscribed.replay_from and truncated.first_available.
func (t *topicState) advance(seq uint64) {
	if !t.hasNext || seq > t.next {
		t.next = seq
		t.hasNext = true
	}
}

// resumeFrom is where a resubscription after a reconnect starts: the next
// expected seq when known, otherwise the original start point.
func (t *topicState) resumeFrom() StartFrom {
	if t.hasNext {
		return FromSeq(t.next)
	}
	return t.from
}

// receive decides what to do with a delivered message. A seq below the
// next expected one is a duplicate: it is not handed over, and is
// acknowledged again if it is QoS 1 and was acknowledged before (or always
// in AckAuto). Anything else moves the next expected seq past it, is handed
// over, and in AckAuto a QoS 1 message is acknowledged now.
func (t *topicState) receive(seq uint64, qos uint8, mode AckMode) (deliver bool, ack bool) {
	if t.hasNext && seq < t.next {
		return false, qos == 1 && (mode == AckAuto || t.acked.contains(seq))
	}
	t.next = seq + 1
	t.hasNext = true
	if qos == 1 && mode == AckAuto {
		t.acked.insert(seq)
		return true, true
	}
	return true, false
}
