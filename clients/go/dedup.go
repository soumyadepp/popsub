package popsub

// ackMemorySize is how many acknowledged seqs a subscription remembers, to
// acknowledge again duplicates the application already acknowledged. A
// shared subscription remembers as many delivered seqs.
const ackMemorySize = 4096

// ackMemory is a bounded set of seqs that forgets the oldest beyond a
// limit: the seqs a subscription acknowledged recently, or the seqs a
// shared subscription handed to the application.
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
	// owner is the wildcard pattern the topic was joined through, or empty
	// when the application subscribed to it by name.
	owner string
	// group is the shared subscription it is read through, or empty. A
	// member gets only some of the topic's messages, out of order, so
	// duplicates are found with seen and next is not used.
	group string
	// seen is the seqs handed to the application, for a shared subscription.
	seen *ackMemory
}

// newTopicState builds the record of a new subscription.
func newTopicState(sub *Subscription, from StartFrom) *topicState {
	return &topicState{sub: sub, from: from, acked: newAckMemory(ackMemorySize), seen: newAckMemory(ackMemorySize)}
}

// advance moves the next expected seq forward to at least seq, for
// subscribed.replay_from and truncated.first_available. It does nothing
// for a shared subscription, whose position is the group's, on the server.
func (t *topicState) advance(seq uint64) {
	if t.group != "" {
		return
	}
	if !t.hasNext || seq > t.next {
		t.next = seq
		t.hasNext = true
	}
}

// resumeFrom is where a resubscription after a reconnect starts: the next
// expected seq when known, otherwise the original start point. A shared
// subscription always rejoins with resume.
func (t *topicState) resumeFrom() StartFrom {
	if t.group != "" {
		return FromResume
	}
	if t.hasNext {
		return FromSeq(t.next)
	}
	return t.from
}

// receive decides what to do with a delivered message. A duplicate is a
// seq below the next expected one, or for a shared subscription a seq
// already handed over: it is not handed over again, and is acknowledged
// again if it is QoS 1 and was acknowledged before (or always in AckAuto).
// Anything else is recorded (the next expected seq moves past it, or the
// seq joins the seen set), is handed over, and in AckAuto a QoS 1 message
// is acknowledged now.
func (t *topicState) receive(seq uint64, qos uint8, mode AckMode) (deliver bool, ack bool) {
	duplicate := t.hasNext && seq < t.next
	if t.group != "" {
		duplicate = t.seen.contains(seq)
	}
	if duplicate {
		return false, qos == 1 && (mode == AckAuto || t.acked.contains(seq))
	}
	if t.group != "" {
		t.seen.insert(seq)
	} else {
		t.next = seq + 1
		t.hasNext = true
	}
	if qos == 1 && mode == AckAuto {
		t.acked.insert(seq)
		return true, true
	}
	return true, false
}
