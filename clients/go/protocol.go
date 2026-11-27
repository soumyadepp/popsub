package popsub

import (
	"bytes"
	"encoding/json"
	"fmt"
	"strconv"
	"time"
)

// protocolVersion is the protocol version this client speaks, sent in auth.
const protocolVersion = 1

// startKind says which form a StartFrom takes.
type startKind int

// The forms of StartFrom.
const (
	// startResume continues a durable session's stored cursor.
	startResume startKind = iota
	// startNew replays nothing.
	startNew
	// startLast replays the last N records.
	startLast
	// startSince replays from a time.
	startSince
	// startSeq replays from a seq.
	startSeq
)

// StartFrom says where replay starts when subscribing. The zero value is
// FromResume, the protocol's default.
type StartFrom struct {
	// kind is the form.
	kind startKind
	// n is the count for startLast, or the seq for startSeq.
	n uint64
	// ms is the time for startSince, in milliseconds since the Unix epoch.
	ms int64
}

// FromResume continues a durable session's stored subscription, one past
// its cursor. For an ephemeral connection, or a session with no stored
// subscription to the topic, it replays nothing.
var FromResume = StartFrom{kind: startResume}

// FromNew replays nothing: only records published after subscribing arrive.
var FromNew = StartFrom{kind: startNew}

// FromLast replays the last n retained records.
func FromLast(n uint64) StartFrom {
	return StartFrom{kind: startLast, n: n}
}

// FromSince replays from the first record published at or after t.
func FromSince(t time.Time) StartFrom {
	return StartFrom{kind: startSince, ms: t.UnixMilli()}
}

// FromSinceMs replays from the first record published at or after ms
// milliseconds since the Unix epoch.
func FromSinceMs(ms int64) StartFrom {
	return StartFrom{kind: startSince, ms: ms}
}

// FromSeq replays from the record with this seq.
func FromSeq(seq uint64) StartFrom {
	return StartFrom{kind: startSeq, n: seq}
}

// String describes the start point as it appears on the wire.
func (s StartFrom) String() string {
	b, _ := s.MarshalJSON()
	return string(b)
}

// MarshalJSON writes the start point as the protocol does: "resume",
// "new", {"last": n}, {"since_ms": ms} or {"seq": n}.
func (s StartFrom) MarshalJSON() ([]byte, error) {
	switch s.kind {
	case startNew:
		return []byte(`"new"`), nil
	case startLast:
		return []byte(`{"last":` + strconv.FormatUint(s.n, 10) + `}`), nil
	case startSince:
		return []byte(`{"since_ms":` + strconv.FormatInt(s.ms, 10) + `}`), nil
	case startSeq:
		return []byte(`{"seq":` + strconv.FormatUint(s.n, 10) + `}`), nil
	default:
		return []byte(`"resume"`), nil
	}
}

// UnmarshalJSON reads any of the protocol's forms of a start point.
func (s *StartFrom) UnmarshalJSON(data []byte) error {
	var word string
	if err := json.Unmarshal(data, &word); err == nil {
		switch word {
		case "resume":
			*s = FromResume
		case "new":
			*s = FromNew
		default:
			return fmt.Errorf("unknown start point %q", word)
		}
		return nil
	}
	var fields struct {
		Last    *uint64 `json:"last"`
		SinceMs *int64  `json:"since_ms"`
		Seq     *uint64 `json:"seq"`
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&fields); err != nil {
		return fmt.Errorf("invalid start point %s: %w", data, err)
	}
	switch {
	case fields.Last != nil && fields.SinceMs == nil && fields.Seq == nil:
		*s = FromLast(*fields.Last)
	case fields.SinceMs != nil && fields.Last == nil && fields.Seq == nil:
		*s = FromSinceMs(*fields.SinceMs)
	case fields.Seq != nil && fields.Last == nil && fields.SinceMs == nil:
		*s = FromSeq(*fields.Seq)
	default:
		return fmt.Errorf("invalid start point %s", data)
	}
	return nil
}

// Access is what a topic grant allows.
type Access string

// The kinds of Access.
const (
	// AccessSubscribe allows subscribing only.
	AccessSubscribe Access = "subscribe"
	// AccessPublish allows publishing only.
	AccessPublish Access = "publish"
	// AccessBoth allows subscribing and publishing.
	AccessBoth Access = "both"
)

// RoleKind is the kind of an account's role.
type RoleKind string

// The kinds of role.
const (
	// RoleAdmin may do everything, admin requests included.
	RoleAdmin RoleKind = "admin"
	// RoleUser may use the topics its grants allow.
	RoleUser RoleKind = "user"
	// RoleReadonly may only subscribe, to the topics its grants allow.
	RoleReadonly RoleKind = "readonly"
)

// TopicGrant is one topic pattern an account may use.
type TopicGrant struct {
	// Pattern is the topic pattern, with + for one level and a trailing #
	// for the rest.
	Pattern string `json:"pattern"`
	// Access is what the pattern allows.
	Access Access `json:"access"`
}

// Role is what an account may do.
type Role struct {
	// Kind is the kind of role.
	Kind RoleKind `json:"kind"`
	// Topics are its grants; ignored for RoleAdmin.
	Topics []TopicGrant `json:"topics,omitempty"`
}

// User is an account, as the admin requests return it. It carries no
// password hash.
type User struct {
	// Username is the account's name.
	Username string `json:"username"`
	// Role is what it may do.
	Role Role `json:"role"`
	// Enabled says whether it may log in.
	Enabled bool `json:"enabled"`
	// CreatedAtMs is when it was created, in milliseconds since the Unix epoch.
	CreatedAtMs int64 `json:"created_at_ms"`
}

// loginRequest is the login frame.
type loginRequest struct {
	// Type is "login".
	Type string `json:"type"`
	// Username is the username.
	Username string `json:"username"`
	// Password is the password.
	Password string `json:"password"`
}

// authRequest is the auth frame.
type authRequest struct {
	// Type is "auth".
	Type string `json:"type"`
	// Token is the JWT.
	Token string `json:"token"`
	// Protocol is the protocol version.
	Protocol int `json:"protocol"`
	// SessionID names a durable session; empty for an ephemeral connection.
	SessionID string `json:"session_id,omitempty"`
	// CleanStart discards the session's stored subscriptions first.
	CleanStart bool `json:"clean_start,omitempty"`
	// HoldResumed asks the server to list the session's stored
	// subscriptions without attaching them, so it sends nothing on them
	// until the client subscribes. Set whenever SessionID is.
	HoldResumed bool `json:"hold_resumed,omitempty"`
}

// subscribeRequest is the subscribe frame.
type subscribeRequest struct {
	// Type is "subscribe".
	Type string `json:"type"`
	// Topic is the topic.
	Topic string `json:"topic"`
	// From is where replay starts.
	From StartFrom `json:"from"`
	// Group is the shared subscription to join, omitted for none.
	Group string `json:"group,omitempty"`
}

// unsubscribeRequest is the unsubscribe frame.
type unsubscribeRequest struct {
	// Type is "unsubscribe".
	Type string `json:"type"`
	// Topic is the topic.
	Topic string `json:"topic"`
}

// publishRequest is the publish frame.
type publishRequest struct {
	// Type is "publish".
	Type string `json:"type"`
	// Topic is the topic.
	Topic string `json:"topic"`
	// Payload is the body: text, or base64 when Encoding says so.
	Payload string `json:"payload"`
	// Encoding is EncodingBase64 for a body published as bytes, and
	// omitted for text.
	Encoding Encoding `json:"encoding,omitempty"`
	// MessageID is the idempotency key of a confirmed publish.
	MessageID string `json:"message_id,omitempty"`
	// QoS is 1 for a confirmed publish, and omitted (0) otherwise.
	QoS uint8 `json:"qos,omitempty"`
}

// ackRequest is the ack frame.
type ackRequest struct {
	// Type is "ack".
	Type string `json:"type"`
	// Topic is the topic the message arrived on.
	Topic string `json:"topic"`
	// Seq is the message's seq.
	Seq uint64 `json:"seq"`
}

// usernameRequest is an admin frame that names only an account: get_user
// and delete_user.
type usernameRequest struct {
	// Type is the frame type.
	Type string `json:"type"`
	// Username is the account.
	Username string `json:"username"`
}

// bareRequest is an admin frame with no fields: list_users and list_topics.
type bareRequest struct {
	// Type is the frame type.
	Type string `json:"type"`
}

// topicRequest is an admin frame that names only a topic: get_topic and
// purge_topic.
type topicRequest struct {
	// Type is the frame type.
	Type string `json:"type"`
	// Topic is the topic.
	Topic string `json:"topic"`
}

// backupRequest is the backup admin frame.
type backupRequest struct {
	// Type is the frame type.
	Type string `json:"type"`
	// Name is the backup's directory name, left out for the default.
	Name string `json:"name,omitempty"`
}

// BackupInfo describes a backup the server wrote of itself, as Admin.Backup
// returns it.
type BackupInfo struct {
	// Name is the backup's directory name.
	Name string `json:"name"`
	// Path is where the backup is on the server's file system.
	Path string `json:"path"`
	// Topics is how many topics it holds.
	Topics uint64 `json:"topics"`
	// Records is how many records it holds across every topic.
	Records uint64 `json:"records"`
	// Bytes is how many bytes were written.
	Bytes uint64 `json:"bytes"`
	// Users says whether accounts were included; false when the server
	// doesn't keep them in users.json.
	Users bool `json:"users"`
}

// TopicInfo describes a topic's log, as the topic admin requests return it.
type TopicInfo struct {
	// Name is the topic.
	Name string `json:"name"`
	// FirstSeq is the oldest seq still stored; one past LastSeq when the
	// log is empty.
	FirstSeq uint64 `json:"first_seq"`
	// LastSeq is the newest seq ever stored.
	LastSeq uint64 `json:"last_seq"`
	// Messages is how many records the log holds.
	Messages uint64 `json:"messages"`
	// Subscribers is how many live subscriptions the topic has.
	Subscribers uint64 `json:"subscribers"`
}

// topicField is a frame's "topic" field, which is a topic name in most
// frames and a topic object in topic_info.
type topicField struct {
	// name is the topic name, or empty when the field is not a string.
	name string
	// raw is the field as it arrived.
	raw json.RawMessage
}

// UnmarshalJSON keeps the raw field and reads it as a name when it is a string.
func (t *topicField) UnmarshalJSON(data []byte) error {
	t.raw = append(t.raw[:0], data...)
	t.name = ""
	_ = json.Unmarshal(data, &t.name)
	return nil
}

// createUserRequest is the create_user frame.
type createUserRequest struct {
	// Type is "create_user".
	Type string `json:"type"`
	// Username is the new account's name.
	Username string `json:"username"`
	// Password is its password.
	Password string `json:"password"`
	// Role is what it may do.
	Role Role `json:"role"`
}

// setRoleRequest is the set_role frame.
type setRoleRequest struct {
	// Type is "set_role".
	Type string `json:"type"`
	// Username is the account.
	Username string `json:"username"`
	// Role is its new role.
	Role Role `json:"role"`
}

// grantRequest is the grant frame.
type grantRequest struct {
	// Type is "grant".
	Type string `json:"type"`
	// Username is the account.
	Username string `json:"username"`
	// Pattern is the topic pattern.
	Pattern string `json:"pattern"`
	// Access is what it allows.
	Access Access `json:"access"`
}

// revokeRequest is the revoke frame.
type revokeRequest struct {
	// Type is "revoke".
	Type string `json:"type"`
	// Username is the account.
	Username string `json:"username"`
	// Pattern is the pattern, exactly as granted.
	Pattern string `json:"pattern"`
}

// setPasswordRequest is the set_password frame.
type setPasswordRequest struct {
	// Type is "set_password".
	Type string `json:"type"`
	// Username is the account.
	Username string `json:"username"`
	// Password is the new password.
	Password string `json:"password"`
}

// setEnabledRequest is the set_enabled frame.
type setEnabledRequest struct {
	// Type is "set_enabled".
	Type string `json:"type"`
	// Username is the account.
	Username string `json:"username"`
	// Enabled says whether it may log in.
	Enabled bool `json:"enabled"`
}

// limits is the limits object of the authenticated frame.
type limits struct {
	// HeartbeatSecs is how often the server pings.
	HeartbeatSecs uint64 `json:"heartbeat_secs"`
	// IdleTimeoutSecs is how long the server waits for anything from the
	// client before closing the connection.
	IdleTimeoutSecs uint64 `json:"idle_timeout_secs"`
}

// silence is how long to wait for anything from the server before deciding
// the connection is dead: the idle timeout (at least one second) plus one
// heartbeat, since the server pings at that interval.
func (l limits) silence() time.Duration {
	idle := l.IdleTimeoutSecs
	if idle < 1 {
		idle = 1
	}
	return time.Duration(idle+l.HeartbeatSecs) * time.Second
}

// serverFrame is any frame from the server. Its fields are the union of
// every frame type's fields; Type says which apply.
type serverFrame struct {
	// ID is the request id the frame answers, absent for frames the server
	// sends on its own.
	ID json.RawMessage `json:"id"`
	// Type is the frame type.
	Type string `json:"type"`
	// Token is the JWT, in logged_in.
	Token string `json:"token"`
	// Limits are the server's limits, in authenticated.
	Limits *limits `json:"limits"`
	// Resumed are the topics a durable session restored, in authenticated.
	Resumed []string `json:"resumed"`
	// Held says the server left the resumed topics unattached, in
	// authenticated; absent means it did not.
	Held bool `json:"held"`
	// ExpiresAtMs is when the token expires, in auth_expiring.
	ExpiresAtMs int64 `json:"expires_at_ms"`
	// Topic is the topic's name, in message, subscribed, unsubscribed,
	// truncated and stalled, and the topic object in topic_info.
	Topic topicField `json:"topic"`
	// Topics are the topic objects, in topics.
	Topics json.RawMessage `json:"topics"`
	// Backup is the backup object, in backup_created.
	Backup json.RawMessage `json:"backup"`
	// ReplayFrom is the first history record, in subscribed.
	ReplayFrom uint64 `json:"replay_from"`
	// Seq is the record's seq, in message, published and stalled.
	Seq uint64 `json:"seq"`
	// MessageID is the publisher's id, in message and published.
	MessageID string `json:"message_id"`
	// Payload is the body, in message.
	Payload string `json:"payload"`
	// Encoding is how Payload encodes the body, in message; absent for text.
	Encoding Encoding `json:"encoding"`
	// QoS is 0 or 1, in message.
	QoS uint8 `json:"qos"`
	// PublishedAtMs is when the record was published, in message.
	PublishedAtMs int64 `json:"published_at_ms"`
	// FirstAvailable is the oldest seq still stored, in truncated.
	FirstAvailable uint64 `json:"first_available"`
	// User is the account, in user.
	User json.RawMessage `json:"user"`
	// Users are the accounts, in users.
	Users json.RawMessage `json:"users"`
	// Code is the error code, in error.
	Code string `json:"code"`
	// Message is the error text, in error.
	Message string `json:"message"`
	// RetryAfter is how many seconds to wait, in error, for limits.
	RetryAfter uint64 `json:"retry_after"`
}

// requestID returns the numeric request id the frame answers, or 0 when it
// has none (or one this client never sends, such as a string).
func (f *serverFrame) requestID() uint64 {
	if len(f.ID) == 0 {
		return 0
	}
	var id uint64
	if err := json.Unmarshal(f.ID, &id); err != nil {
		return 0
	}
	return id
}

// unprompted reports whether the frame carries no id, meaning the server
// sent it on its own rather than in reply to a request.
func (f *serverFrame) unprompted() bool {
	return len(f.ID) == 0 || string(f.ID) == "null"
}

// encode writes a request as a JSON text frame, with "id" first when id is
// not 0.
func encode(id uint64, message any) ([]byte, error) {
	body, err := json.Marshal(message)
	if err != nil {
		return nil, err
	}
	if id == 0 {
		return body, nil
	}
	out := make([]byte, 0, len(body)+24)
	out = append(out, `{"id":`...)
	out = strconv.AppendUint(out, id, 10)
	out = append(out, ',')
	return append(out, body[1:]...), nil
}
