package popsub

import "sync"

// commandQueue is an unbounded queue of commands for the engine. Pushing
// never blocks, so Message.Ack can be called from any goroutine even while
// the engine waits on a full subscription buffer.
type commandQueue struct {
	// mu guards items and closed.
	mu sync.Mutex
	// items are the queued commands, oldest first.
	items []any
	// closed is set once the engine stopped.
	closed bool
	// signal holds a token while items may be non-empty.
	signal chan struct{}
}

// newCommandQueue builds an empty queue.
func newCommandQueue() *commandQueue {
	return &commandQueue{signal: make(chan struct{}, 1)}
}

// push adds a command, or reports false when the engine has stopped.
func (q *commandQueue) push(command any) bool {
	q.mu.Lock()
	if q.closed {
		q.mu.Unlock()
		return false
	}
	q.items = append(q.items, command)
	q.mu.Unlock()
	select {
	case q.signal <- struct{}{}:
	default:
	}
	return true
}

// pop takes the oldest command, or reports false when there is none.
func (q *commandQueue) pop() (any, bool) {
	q.mu.Lock()
	defer q.mu.Unlock()
	if len(q.items) == 0 {
		return nil, false
	}
	command := q.items[0]
	q.items[0] = nil
	q.items = q.items[1:]
	return command, true
}

// close refuses further pushes and returns what was still queued.
func (q *commandQueue) close() []any {
	q.mu.Lock()
	defer q.mu.Unlock()
	q.closed = true
	rest := q.items
	q.items = nil
	return rest
}
