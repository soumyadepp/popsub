package popsub

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"sync"
	"testing"

	"github.com/coder/websocket"
)

// fakeServer is a small in-process stand-in for a PopSub server, enough
// for unit tests: it logs in any password except "wrong", authenticates,
// confirms subscribes (except to topics named "hang", which it never
// answers), confirms publishes and delivers them to the same connection,
// answers unsubscribe and list_users, and lets a test write raw frames.
type fakeServer struct {
	// server is the HTTP server.
	server *httptest.Server
	// mu guards conn and seqs.
	mu sync.Mutex
	// conn is the latest authenticated connection.
	conn *websocket.Conn
	// seqs is the last seq per topic.
	seqs map[string]uint64
	// authenticated gets a token each time a connection authenticates.
	authenticated chan struct{}
}

// newFakeServer starts a fake server and stops it when the test ends.
func newFakeServer(t *testing.T) *fakeServer {
	t.Helper()
	f := &fakeServer{seqs: make(map[string]uint64), authenticated: make(chan struct{}, 16)}
	f.server = httptest.NewServer(http.HandlerFunc(f.handle))
	t.Cleanup(f.server.Close)
	return f
}

// url is the server's WebSocket address.
func (f *fakeServer) url() string {
	return "ws" + strings.TrimPrefix(f.server.URL, "http")
}

// handle serves one WebSocket connection until it closes.
func (f *fakeServer) handle(w http.ResponseWriter, r *http.Request) {
	conn, err := websocket.Accept(w, r, nil)
	if err != nil {
		return
	}
	defer conn.CloseNow()
	ctx := context.Background()
	for {
		_, data, err := conn.Read(ctx)
		if err != nil {
			return
		}
		var frame map[string]any
		if json.Unmarshal(data, &frame) != nil {
			continue
		}
		f.answer(ctx, conn, frame)
	}
}

// answer replies to one client frame.
func (f *fakeServer) answer(ctx context.Context, conn *websocket.Conn, frame map[string]any) {
	id := frame["id"]
	reply := func(fields map[string]any) {
		if id != nil {
			fields["id"] = id
		}
		data, _ := json.Marshal(fields)
		_ = conn.Write(ctx, websocket.MessageText, data)
	}
	topic, _ := frame["topic"].(string)
	switch frame["type"] {
	case "login":
		if frame["password"] == "wrong" {
			reply(map[string]any{"type": "error", "code": "invalid_credentials", "message": "wrong username or password"})
			return
		}
		reply(map[string]any{"type": "logged_in", "token": "token"})
	case "auth":
		f.mu.Lock()
		f.conn = conn
		f.mu.Unlock()
		reply(map[string]any{"type": "authenticated", "protocol": 1, "resumed": []string{},
			"limits": map[string]any{"heartbeat_secs": 30, "idle_timeout_secs": 90}})
		f.authenticated <- struct{}{}
	case "subscribe":
		if topic == "hang" {
			return
		}
		f.mu.Lock()
		next := f.seqs[topic] + 1
		f.mu.Unlock()
		reply(map[string]any{"type": "subscribed", "topic": topic, "replay_from": next, "replay_to": next - 1})
	case "unsubscribe":
		reply(map[string]any{"type": "unsubscribed", "topic": topic})
	case "publish":
		f.mu.Lock()
		f.seqs[topic]++
		seq := f.seqs[topic]
		f.mu.Unlock()
		messageID, _ := frame["message_id"].(string)
		if messageID == "" {
			messageID = "generated-" + strconv.FormatUint(seq, 10)
		}
		qos, _ := frame["qos"].(float64)
		if qos == 1 {
			reply(map[string]any{"type": "published", "message_id": messageID, "seq": seq})
		}
		f.send(ctx, conn, map[string]any{"type": "message", "topic": topic, "seq": seq, "message_id": messageID,
			"payload": frame["payload"], "qos": qos, "published_at_ms": 1794800000000})
	case "list_users":
		reply(map[string]any{"type": "users", "users": []any{}})
	}
}

// send writes a frame the server sends on its own.
func (f *fakeServer) send(ctx context.Context, conn *websocket.Conn, fields map[string]any) {
	data, _ := json.Marshal(fields)
	_ = conn.Write(ctx, websocket.MessageText, data)
}

// inject writes raw text to the latest authenticated connection.
func (f *fakeServer) inject(t *testing.T, text string) {
	t.Helper()
	f.mu.Lock()
	conn := f.conn
	f.mu.Unlock()
	if conn == nil {
		t.Fatal("no authenticated connection to write to")
	}
	if err := conn.Write(context.Background(), websocket.MessageText, []byte(text)); err != nil {
		t.Fatalf("injecting a frame: %v", err)
	}
}

// dropConnection closes the latest connection with a close code.
func (f *fakeServer) dropConnection(code websocket.StatusCode) {
	f.mu.Lock()
	conn := f.conn
	f.mu.Unlock()
	if conn != nil {
		_ = conn.Close(code, "")
	}
}
