package popsub

import (
	"context"
	"encoding/json"
	"math/rand/v2"
	"sort"
	"sync/atomic"
	"time"

	"github.com/coder/websocket"
)

// offlinePublishes is how many QoS 0 publishes are held while disconnected;
// older ones are dropped first.
const offlinePublishes = 10000

// defaultSilence is how long the engine waits for anything from the server
// before deciding the connection is dead, until the server announces its
// own limits.
const defaultSilence = 120 * time.Second

// readLimit is the largest frame the client accepts, the same as the Rust
// client's WebSocket library allows by default.
const readLimit = 64 << 20

// Published is the result of a confirmed publish.
type Published struct {
	// Seq is the message's position in the topic.
	Seq uint64
	// MessageID is the id it was stored under.
	MessageID string
}

// subscribeCommand asks the engine to subscribe and report the subscribed frame.
type subscribeCommand struct {
	// topic is the topic.
	topic string
	// from is where replay starts.
	from StartFrom
	// sub is the application's subscription.
	sub *Subscription
	// reply gets nil once subscribed, or the error.
	reply chan *Error
}

// unsubscribeCommand asks the engine to unsubscribe and report unsubscribed.
type unsubscribeCommand struct {
	// topic is the topic.
	topic string
	// reply gets nil once unsubscribed, or the error.
	reply chan *Error
}

// forgetCommand asks the engine to unsubscribe without reporting, if the
// topic still belongs to sub, after a Subscribe whose caller stopped waiting.
type forgetCommand struct {
	// topic is the topic.
	topic string
	// sub is the subscription being given up.
	sub *Subscription
}

// publishCommand asks the engine to publish a message.
type publishCommand struct {
	// topic is the topic.
	topic string
	// payload is the body.
	payload string
	// messageID is the idempotency key of a confirmed publish.
	messageID string
	// reply gets the outcome of a confirmed publish; nil for QoS 0.
	reply chan publishResult
}

// ackCommand asks the engine to acknowledge a message, from Message.Ack.
type ackCommand struct {
	// topic is the topic.
	topic string
	// seq is the message's seq.
	seq uint64
}

// requestCommand asks the engine to send an admin request and report its
// reply. It is never queued while disconnected and never resent, because
// it may not be safe to repeat.
type requestCommand struct {
	// message is the request.
	message any
	// reply gets the reply frame, or the error.
	reply chan requestResult
}

// closeCommand asks the engine to close the connection and stop.
type closeCommand struct{}

// publishResult is the outcome of a confirmed publish.
type publishResult struct {
	// published is the result, when err is nil.
	published Published
	// err is the failure, if any.
	err *Error
}

// requestResult is the outcome of an admin request.
type requestResult struct {
	// frame is the reply, when err is nil.
	frame *serverFrame
	// err is the failure, if any.
	err *Error
}

// pendingSubscribe is a subscribe waiting for its reply; the caller waits
// in engine.waiters.
type pendingSubscribe struct {
	// topic is the topic.
	topic string
}

// pendingUnsubscribe is an unsubscribe waiting for its reply.
type pendingUnsubscribe struct {
	// reply gets the outcome.
	reply chan *Error
}

// pendingPublish is a confirmed publish waiting for published; its frame
// is in engine.unconfirmed for resending.
type pendingPublish struct {
	// reply gets the outcome.
	reply chan publishResult
}

// pendingRequest is an admin request waiting for its reply.
type pendingRequest struct {
	// reply gets the outcome.
	reply chan requestResult
}

// pendingRelogin is a login sent to refresh the token on the current connection.
type pendingRelogin struct{}

// pendingReauth is the auth that installs the refreshed token.
type pendingReauth struct{}

// connectFailure is why a connection attempt failed.
type connectFailure struct {
	// err is the error to report.
	err *Error
	// retry says whether trying again could help.
	retry bool
}

// ending is how a connection ended.
type ending struct {
	// stop is true when the application closed the client.
	stop bool
	// code is the server's close code when the connection was lost, or 0.
	code CloseCode
}

// readResult is one read from the socket: a text frame, or the error that
// ended reading.
type readResult struct {
	// data is the frame's text.
	data []byte
	// err ends reading.
	err error
}

// engine owns the WebSocket of one Client. One goroutine runs it: it
// connects, logs in and authenticates, serves the connection, and
// reconnects with jittered backoff when the close code allows. After a
// reconnect it resubscribes every topic the server did not restore from the
// next seq it expects, and resends unconfirmed publishes under the same
// request id and message_id. Every field below except lastHeard is touched
// only by that goroutine, until done is closed.
type engine struct {
	// cfg is the configuration, with defaults applied.
	cfg Config
	// queue holds commands from the Client and from Message.Ack.
	queue *commandQueue
	// events is where events go; closed when the engine stops.
	events chan Event
	// done is closed when the engine has stopped.
	done chan struct{}
	// ready gets the first connection's outcome; nil once sent.
	ready chan *Error
	// ctx is cancelled by Client.Close, to cut short dialing, handshakes and
	// deliveries waiting on a full buffer.
	ctx context.Context
	// cancel cancels ctx.
	cancel context.CancelFunc
	// stopCode is the close code that stopped the engine; read after done.
	stopCode CloseCode
	// topics are the subscribed topics.
	topics map[string]*topicState
	// waiters are subscribes waiting for subscribed, by topic.
	waiters map[string]chan *Error
	// pending are requests waiting for a reply, by request id.
	pending map[uint64]any
	// unconfirmed are confirmed publishes not yet confirmed, by request id.
	unconfirmed map[uint64]publishRequest
	// offline are frames to send once connected: QoS 0 publishes and unsubscribes.
	offline []any
	// nextID is the next request id.
	nextID uint64
	// connectedOnce is set after the first connection, so clean_start is sent once.
	connectedOnce bool
	// silence is how long to wait for anything from the server.
	silence time.Duration
	// lastHeard is when anything last arrived from the server, pings
	// included, in Unix nanoseconds.
	lastHeard atomic.Int64
}

// newEngine builds an engine that has not connected yet.
func newEngine(cfg Config) *engine {
	ctx, cancel := context.WithCancel(context.Background())
	return &engine{
		cfg:         cfg,
		queue:       newCommandQueue(),
		events:      make(chan Event, eventBuffer),
		done:        make(chan struct{}),
		ready:       make(chan *Error, 1),
		ctx:         ctx,
		cancel:      cancel,
		topics:      make(map[string]*topicState),
		waiters:     make(map[string]chan *Error),
		pending:     make(map[uint64]any),
		unconfirmed: make(map[uint64]publishRequest),
		nextID:      1,
		silence:     defaultSilence,
	}
}

// run connects, serves each connection, reconnects with backoff, and
// reports the first connection's outcome on ready, until the client stops.
func (e *engine) run() {
	attempt := 0
	var lastErr *Error
	var final CloseCode
	for {
		conn, resumed, failure := e.connect()
		if failure == nil {
			attempt = 0
			e.connectedOnce = true
			e.signalReady(nil)
			e.emit(ConnectedEvent{Resumed: resumed})
			end := e.serve(conn, resumed)
			if end.stop {
				break
			}
			e.failRequests()
			reconnecting := e.cfg.Reconnect.retries() && e.shouldReconnect(end.code)
			e.emit(DisconnectedEvent{Code: end.code, Reconnecting: reconnecting})
			if !reconnecting {
				final = end.code
				break
			}
		} else {
			lastErr = failure.err
			if !failure.retry {
				e.signalReady(failure.err)
				break
			}
			if e.ctx.Err() != nil {
				break
			}
		}
		attempt++
		if e.cfg.Reconnect.exhausted(attempt) {
			var cause error
			if lastErr != nil {
				cause = lastErr
			}
			reason := "ran out of reconnect attempts; the last attempt failed"
			if !e.cfg.Reconnect.retries() {
				reason = "reconnecting is off and the attempt failed"
			}
			e.signalReady(connectError(reason, cause))
			break
		}
		if !e.waitBeforeRetry(attempt) {
			break
		}
	}
	e.stop(final)
}

// signalReady reports the first connection's outcome, once.
func (e *engine) signalReady(err *Error) {
	if e.ready != nil {
		e.ready <- err
		e.ready = nil
	}
}

// shouldReconnect decides whether to reconnect after a connection was
// lost: by the close code, except that an expired token only allows it
// with password credentials, which log in again.
func (e *engine) shouldReconnect(code CloseCode) bool {
	if code == CloseTokenExpired {
		return e.cfg.Credentials.usesPassword()
	}
	return code.shouldReconnect()
}

// waitBeforeRetry waits a random time up to the backoff ceiling, handling
// commands meanwhile as offline commands. It reports false when the
// application closed the client while waiting.
func (e *engine) waitBeforeRetry(attempt int) bool {
	ceiling := e.cfg.Reconnect.Ceiling(attempt)
	delay := time.Duration(rand.Float64() * float64(ceiling))
	timer := time.NewTimer(delay)
	defer timer.Stop()
	for {
		select {
		case <-timer.C:
			return true
		case <-e.queue.signal:
			for {
				command, ok := e.queue.pop()
				if !ok {
					break
				}
				if _, closing := command.(closeCommand); closing {
					return false
				}
				e.queueOffline(command)
			}
		}
	}
}

// failRequests fails every admin request still waiting when a connection
// is lost, since the server may or may not have carried it out.
func (e *engine) failRequests() {
	for id, p := range e.pending {
		if request, ok := p.(pendingRequest); ok {
			delete(e.pending, id)
			request.reply <- requestResult{err: connectError("the connection dropped before the reply; the request may or may not have been carried out", nil)}
		}
	}
}

// takeID returns a new request id.
func (e *engine) takeID() uint64 {
	id := e.nextID
	e.nextID++
	return id
}

// quietID returns a request id for a frame the engine sends on its own
// without waiting for the reply: an unsubscribe always gets one, so its
// unsubscribed reply can't be mistaken for the server ending the
// subscription, which it announces with no id. Other frames get 0.
func (e *engine) quietID(message any) uint64 {
	if _, ok := message.(unsubscribeRequest); ok {
		return e.takeID()
	}
	return 0
}

// heard records that something arrived from the server.
func (e *engine) heard() {
	e.lastHeard.Store(time.Now().UnixNano())
}

// connect opens a connection, logs in when the credentials hold a
// password, and authenticates. It returns the connection and the topics
// the server restored for the session, or why it failed: not retryable for
// rejected credentials, an unsupported protocol or a bad session id.
func (e *engine) connect() (*websocket.Conn, []string, *connectFailure) {
	ctx, cancel := context.WithTimeout(e.ctx, e.cfg.RequestTimeout)
	defer cancel()
	conn, _, err := websocket.Dial(ctx, e.cfg.URL, &websocket.DialOptions{
		HTTPClient: e.cfg.HTTPClient,
		OnPingReceived: func(context.Context, []byte) bool {
			e.heard()
			return true
		},
	})
	if err != nil {
		return nil, nil, &connectFailure{err: connectError("dialing failed", err), retry: true}
	}
	conn.SetReadLimit(readLimit)
	token := e.cfg.Credentials.Token
	if e.cfg.Credentials.usesPassword() {
		login := loginRequest{Type: "login", Username: e.cfg.Credentials.Username, Password: e.cfg.Credentials.Password}
		reply, failure := e.exchange(ctx, conn, e.takeID(), login)
		if failure != nil {
			conn.CloseNow()
			return nil, nil, failure
		}
		if reply.Type != "logged_in" {
			conn.CloseNow()
			return nil, nil, refusal(reply)
		}
		token = reply.Token
	}
	auth := authRequest{
		Type:       "auth",
		Token:      token,
		Protocol:   protocolVersion,
		SessionID:  e.cfg.SessionID,
		CleanStart: e.cfg.CleanStart && !e.connectedOnce,
	}
	reply, failure := e.exchange(ctx, conn, e.takeID(), auth)
	if failure != nil {
		conn.CloseNow()
		return nil, nil, failure
	}
	if reply.Type != "authenticated" {
		conn.CloseNow()
		return nil, nil, refusal(reply)
	}
	e.silence = defaultSilence
	if reply.Limits != nil {
		e.silence = reply.Limits.silence()
	}
	resumed := reply.Resumed
	if resumed == nil {
		resumed = []string{}
	}
	return conn, resumed, nil
}

// exchange sends a request while connecting and waits for the frame that
// answers it, skipping others. A close from the server is a KindClosed
// failure, retryable unless its code rules reconnecting out.
func (e *engine) exchange(ctx context.Context, conn *websocket.Conn, id uint64, message any) (*serverFrame, *connectFailure) {
	data, err := encode(id, message)
	if err != nil {
		return nil, &connectFailure{err: protocolError(err.Error()), retry: false}
	}
	if err := conn.Write(ctx, websocket.MessageText, data); err != nil {
		return nil, lostDuringConnect(ctx, err)
	}
	for {
		kind, data, err := conn.Read(ctx)
		if err != nil {
			return nil, lostDuringConnect(ctx, err)
		}
		e.heard()
		if kind != websocket.MessageText {
			continue
		}
		var frame serverFrame
		if json.Unmarshal(data, &frame) != nil {
			continue
		}
		if frame.requestID() == id {
			return &frame, nil
		}
	}
}

// lostDuringConnect describes a connection that failed while connecting:
// closed by the server, timed out, or dropped.
func lostDuringConnect(ctx context.Context, err error) *connectFailure {
	if status := websocket.CloseStatus(err); status != -1 {
		code := knownCloseCode(int(status))
		return &connectFailure{err: closedError(code), retry: code.shouldReconnect()}
	}
	if ctx.Err() != nil {
		return &connectFailure{err: timeoutError(ctx.Err()), retry: true}
	}
	return &connectFailure{err: connectError("the connection dropped", err), retry: true}
}

// refusal turns an unexpected reply while connecting into a failure,
// retryable only for the server's temporary refusals.
func refusal(reply *serverFrame) *connectFailure {
	if reply.Type == "error" {
		err := serverError(reply)
		return &connectFailure{err: err, retry: err.Temporary()}
	}
	return &connectFailure{err: protocolError("expected a reply to login or auth, got " + reply.Type), retry: true}
}

// closeCodeOf returns the PopSub close code an error from reading carries,
// or 0.
func closeCodeOf(err error) CloseCode {
	status := websocket.CloseStatus(err)
	if status == -1 {
		return 0
	}
	return knownCloseCode(int(status))
}

// serve runs one authenticated connection until it ends: it restores
// subscriptions and resends first, then handles frames, commands and the
// silence limit. It returns only after the connection is closed and its
// reader goroutine has exited.
func (e *engine) serve(conn *websocket.Conn, resumed []string) ending {
	if e.restore(conn, resumed) != nil {
		conn.CloseNow()
		return ending{}
	}
	readCtx, cancel := context.WithCancel(context.Background())
	frames := make(chan readResult)
	readerDone := make(chan struct{})
	e.heard()
	go func() {
		defer close(readerDone)
		e.read(readCtx, conn, frames)
	}()
	defer func() {
		cancel()
		conn.CloseNow()
		<-readerDone
	}()
	timer := time.NewTimer(e.silence)
	defer timer.Stop()
	for {
		select {
		case result := <-frames:
			if result.err != nil {
				conn.CloseNow()
				return ending{code: closeCodeOf(result.err)}
			}
			var frame serverFrame
			if json.Unmarshal(result.data, &frame) != nil {
				continue
			}
			if e.onFrame(conn, &frame) != nil {
				conn.CloseNow()
				return ending{}
			}
		case <-e.queue.signal:
			for {
				command, ok := e.queue.pop()
				if !ok {
					break
				}
				if _, closing := command.(closeCommand); closing {
					conn.Close(websocket.StatusNormalClosure, "")
					return ending{stop: true}
				}
				if e.onCommand(conn, command) != nil {
					conn.CloseNow()
					return ending{}
				}
			}
		case <-timer.C:
			quiet := time.Since(time.Unix(0, e.lastHeard.Load()))
			if quiet >= e.silence {
				conn.CloseNow()
				return ending{}
			}
			timer.Reset(e.silence - quiet)
		}
	}
}

// read reads the socket until it fails or ctx ends, recording when
// anything arrived and passing text frames to out. Because it waits for
// the engine to take each frame, a delivery blocked on a full subscription
// buffer stops it reading, and the server's backpressure paces the
// connection. Reading also answers the server's pings.
func (e *engine) read(ctx context.Context, conn *websocket.Conn, out chan<- readResult) {
	for {
		kind, data, err := conn.Read(ctx)
		if err != nil {
			select {
			case out <- readResult{err: err}:
			case <-ctx.Done():
			}
			return
		}
		e.heard()
		if kind != websocket.MessageText {
			continue
		}
		select {
		case out <- readResult{data: data}:
		case <-ctx.Done():
			return
		}
	}
}

// send writes one frame, with a request id when id is not 0.
func (e *engine) send(conn *websocket.Conn, id uint64, message any) error {
	data, err := encode(id, message)
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), e.cfg.RequestTimeout)
	defer cancel()
	return conn.Write(ctx, websocket.MessageText, data)
}

// restore prepares a new connection: it resubscribes every topic the
// server did not restore for the session from the next seq it expects (so
// nothing still in the log is missed), resends unconfirmed publishes in
// request order under their original ids, and sends queued frames.
func (e *engine) restore(conn *websocket.Conn, resumed []string) error {
	restored := make(map[string]bool, len(resumed))
	for _, topic := range resumed {
		restored[topic] = true
	}
	for name, topic := range e.topics {
		if restored[name] {
			continue
		}
		id := e.takeID()
		e.pending[id] = pendingSubscribe{topic: name}
		if err := e.send(conn, id, subscribeRequest{Type: "subscribe", Topic: name, From: topic.resumeFrom()}); err != nil {
			return err
		}
	}
	ids := make([]uint64, 0, len(e.unconfirmed))
	for id := range e.unconfirmed {
		ids = append(ids, id)
	}
	sort.Slice(ids, func(i, j int) bool { return ids[i] < ids[j] })
	for _, id := range ids {
		if err := e.send(conn, id, e.unconfirmed[id]); err != nil {
			return err
		}
	}
	return e.flushOffline(conn)
}

// flushOffline sends the queued frames, oldest first.
func (e *engine) flushOffline(conn *websocket.Conn) error {
	for len(e.offline) > 0 {
		message := e.offline[0]
		e.offline[0] = nil
		e.offline = e.offline[1:]
		if err := e.send(conn, e.quietID(message), message); err != nil {
			return err
		}
	}
	return nil
}

// queueOffline records a command that arrived while disconnected, to act
// on after reconnecting. Admin requests fail at once instead.
func (e *engine) queueOffline(command any) {
	switch c := command.(type) {
	case subscribeCommand:
		e.addTopic(c.topic, c.from, c.sub)
		e.setWaiter(c.topic, c.reply)
	case unsubscribeCommand:
		e.removeTopic(c.topic)
		e.offline = append(e.offline, unsubscribeRequest{Type: "unsubscribe", Topic: c.topic})
		c.reply <- nil
	case forgetCommand:
		if e.forget(c.topic, c.sub) {
			e.offline = append(e.offline, unsubscribeRequest{Type: "unsubscribe", Topic: c.topic})
		}
	case publishCommand:
		e.queuePublish(c)
	case ackCommand:
		if topic := e.topics[c.topic]; topic != nil {
			topic.acked.insert(c.seq)
		}
	case requestCommand:
		c.reply <- requestResult{err: connectError("not connected; this request isn't queued while disconnected", nil)}
	}
}

// queuePublish registers a publish: a confirmed one waits in unconfirmed
// for its published frame and its request id is returned; a QoS 0 one
// waits in offline until sent, and 0 is returned.
func (e *engine) queuePublish(c publishCommand) uint64 {
	if c.reply != nil {
		id := e.takeID()
		e.pending[id] = pendingPublish{reply: c.reply}
		e.unconfirmed[id] = publishRequest{Type: "publish", Topic: c.topic, Payload: c.payload, MessageID: c.messageID, QoS: 1}
		return id
	}
	if len(e.offline) >= offlinePublishes {
		e.offline[0] = nil
		e.offline = e.offline[1:]
	}
	e.offline = append(e.offline, publishRequest{Type: "publish", Topic: c.topic, Payload: c.payload})
	return 0
}

// setWaiter records who waits for a topic's subscribed frame. A subscribe
// still waiting for the same topic is answered with success: its
// Subscription was replaced by the new one, so its channel is closed.
func (e *engine) setWaiter(topic string, reply chan *Error) {
	if old, ok := e.waiters[topic]; ok {
		old <- nil
	}
	e.waiters[topic] = reply
}

// addTopic adds or replaces a subscribed topic. A replaced subscription's
// channel is closed.
func (e *engine) addTopic(topic string, from StartFrom, sub *Subscription) {
	if old := e.topics[topic]; old != nil && old.sub != sub {
		old.sub.end()
	}
	e.topics[topic] = newTopicState(sub, from)
}

// removeTopic drops a subscribed topic and closes its channel, reporting
// whether it was subscribed.
func (e *engine) removeTopic(topic string) bool {
	state := e.topics[topic]
	if state == nil {
		return false
	}
	state.sub.end()
	delete(e.topics, topic)
	return true
}

// forget drops a topic only if it still belongs to sub, reporting whether
// it did.
func (e *engine) forget(topic string, sub *Subscription) bool {
	if state := e.topics[topic]; state != nil && state.sub == sub {
		return e.removeTopic(topic)
	}
	return false
}

// onCommand acts on a command while connected.
func (e *engine) onCommand(conn *websocket.Conn, command any) error {
	switch c := command.(type) {
	case subscribeCommand:
		e.addTopic(c.topic, c.from, c.sub)
		e.setWaiter(c.topic, c.reply)
		id := e.takeID()
		e.pending[id] = pendingSubscribe{topic: c.topic}
		return e.send(conn, id, subscribeRequest{Type: "subscribe", Topic: c.topic, From: c.from})
	case unsubscribeCommand:
		e.removeTopic(c.topic)
		id := e.takeID()
		e.pending[id] = pendingUnsubscribe{reply: c.reply}
		return e.send(conn, id, unsubscribeRequest{Type: "unsubscribe", Topic: c.topic})
	case forgetCommand:
		if e.forget(c.topic, c.sub) {
			return e.send(conn, e.takeID(), unsubscribeRequest{Type: "unsubscribe", Topic: c.topic})
		}
		return nil
	case publishCommand:
		if id := e.queuePublish(c); id != 0 {
			return e.send(conn, id, e.unconfirmed[id])
		}
		return e.flushOffline(conn)
	case ackCommand:
		if topic := e.topics[c.topic]; topic != nil {
			topic.acked.insert(c.seq)
		}
		return e.send(conn, 0, ackRequest{Type: "ack", Topic: c.topic, Seq: c.seq})
	case requestCommand:
		id := e.takeID()
		e.pending[id] = pendingRequest{reply: c.reply}
		return e.send(conn, id, c.message)
	}
	return nil
}

// onFrame acts on a frame from the server.
func (e *engine) onFrame(conn *websocket.Conn, frame *serverFrame) error {
	id := frame.requestID()
	var pending any
	if id != 0 {
		if p, ok := e.pending[id]; ok {
			pending = p
			delete(e.pending, id)
		}
	}
	if request, ok := pending.(pendingRequest); ok {
		if frame.Type == "error" {
			request.reply <- requestResult{err: serverError(frame)}
		} else {
			request.reply <- requestResult{frame: frame}
		}
		return nil
	}
	switch frame.Type {
	case "message":
		return e.deliver(conn, frame)
	case "subscribed":
		if topic := e.topics[frame.Topic]; topic != nil {
			topic.advance(frame.ReplayFrom)
		}
		if waiter, ok := e.waiters[frame.Topic]; ok {
			delete(e.waiters, frame.Topic)
			waiter <- nil
		}
	case "unsubscribed":
		if unsubscribe, ok := pending.(pendingUnsubscribe); ok {
			unsubscribe.reply <- nil
		} else if frame.unprompted() && e.removeTopic(frame.Topic) {
			e.emit(UnsubscribedEvent{Topic: frame.Topic})
		}
	case "published":
		if publish, ok := pending.(pendingPublish); ok {
			delete(e.unconfirmed, id)
			publish.reply <- publishResult{published: Published{Seq: frame.Seq, MessageID: frame.MessageID}}
		}
	case "truncated":
		if topic := e.topics[frame.Topic]; topic != nil {
			topic.advance(frame.FirstAvailable)
		}
		e.emit(TruncatedEvent{Topic: frame.Topic, FirstAvailable: frame.FirstAvailable})
	case "stalled":
		e.emit(StalledEvent{Topic: frame.Topic, Seq: frame.Seq})
	case "auth_expiring":
		e.emit(AuthExpiringEvent{ExpiresAtMs: frame.ExpiresAtMs})
		if e.cfg.Credentials.usesPassword() {
			relogin := e.takeID()
			e.pending[relogin] = pendingRelogin{}
			return e.send(conn, relogin, loginRequest{Type: "login", Username: e.cfg.Credentials.Username, Password: e.cfg.Credentials.Password})
		}
	case "logged_in":
		if _, ok := pending.(pendingRelogin); ok {
			reauth := e.takeID()
			e.pending[reauth] = pendingReauth{}
			return e.send(conn, reauth, authRequest{Type: "auth", Token: frame.Token, Protocol: protocolVersion})
		}
	case "authenticated":
		if _, ok := pending.(pendingReauth); ok {
			e.emit(AuthRefreshedEvent{})
		}
	case "error":
		e.onError(id, pending, frame)
	}
	return nil
}

// onError routes an error frame to the request it answers, or reports it
// as a ServerErrorEvent when it answers none the application is waiting on.
func (e *engine) onError(id uint64, pending any, frame *serverFrame) {
	err := serverError(frame)
	switch p := pending.(type) {
	case pendingSubscribe:
		e.removeTopic(p.topic)
		if waiter, ok := e.waiters[p.topic]; ok {
			delete(e.waiters, p.topic)
			waiter <- err
		}
	case pendingUnsubscribe:
		p.reply <- err
	case pendingPublish:
		delete(e.unconfirmed, id)
		p.reply <- publishResult{err: err}
	default:
		e.emit(ServerErrorEvent{Code: err.Code, Message: err.Message})
	}
}

// deliver hands a message to its subscription, dropping duplicates and
// acknowledging as the AckMode says. While the subscription's buffer is
// full it waits, so the engine stops taking frames from the reader.
func (e *engine) deliver(conn *websocket.Conn, frame *serverFrame) error {
	ack := ackRequest{Type: "ack", Topic: frame.Topic, Seq: frame.Seq}
	topic := e.topics[frame.Topic]
	if topic == nil {
		if frame.QoS == 1 {
			return e.send(conn, 0, ack)
		}
		return nil
	}
	handOver, acknowledge := topic.receive(frame.Seq, frame.QoS, e.cfg.AckMode)
	if acknowledge {
		if err := e.send(conn, 0, ack); err != nil {
			return err
		}
	}
	if !handOver {
		return nil
	}
	message := Message{
		Topic:         frame.Topic,
		Seq:           frame.Seq,
		MessageID:     frame.MessageID,
		Payload:       frame.Payload,
		QoS:           frame.QoS,
		PublishedAtMs: frame.PublishedAtMs,
	}
	if frame.QoS == 1 && e.cfg.AckMode == AckManual {
		queue, name, seq := e.queue, frame.Topic, frame.Seq
		message.ack = func() { queue.push(ackCommand{topic: name, seq: seq}) }
	}
	sub := topic.sub
	if sub.halted() || e.ctx.Err() != nil {
		return nil
	}
	select {
	case sub.messages <- message:
	case <-sub.stop:
	case <-e.ctx.Done():
	}
	return nil
}

// emit sends an event without blocking, dropping the oldest unread event
// when the buffer is full.
func (e *engine) emit(event Event) {
	for {
		select {
		case e.events <- event:
			return
		default:
		}
		select {
		case <-e.events:
		default:
		}
	}
}

// stop ends the engine for good: it records why, fails everything still
// waiting, closes every subscription's channel and the events channel, and
// closes done.
func (e *engine) stop(code CloseCode) {
	e.stopCode = code
	e.cancel()
	for topic, waiter := range e.waiters {
		delete(e.waiters, topic)
		waiter <- closedError(code)
	}
	for id, p := range e.pending {
		delete(e.pending, id)
		failPending(p, code)
	}
	for name := range e.topics {
		e.removeTopic(name)
	}
	for _, command := range e.queue.close() {
		failCommand(command, code)
	}
	e.signalReady(closedError(code))
	close(e.events)
	close(e.done)
}

// failPending fails a request that was waiting when the engine stopped.
func failPending(p any, code CloseCode) {
	switch p := p.(type) {
	case pendingUnsubscribe:
		p.reply <- closedError(code)
	case pendingPublish:
		p.reply <- publishResult{err: closedError(code)}
	case pendingRequest:
		p.reply <- requestResult{err: closedError(code)}
	}
}

// failCommand fails a command still queued when the engine stopped.
func failCommand(command any, code CloseCode) {
	switch c := command.(type) {
	case subscribeCommand:
		c.reply <- closedError(code)
	case unsubscribeCommand:
		c.reply <- closedError(code)
	case publishCommand:
		if c.reply != nil {
			c.reply <- publishResult{err: closedError(code)}
		}
	case requestCommand:
		c.reply <- requestResult{err: closedError(code)}
	}
}
