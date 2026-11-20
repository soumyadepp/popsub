// Package popsub is a Go client for PopSub, a WebSocket publish/subscribe
// broker.
//
// A Client keeps a connection to a PopSub server and does the protocol's
// bookkeeping: logging in and refreshing the token, reconnecting with
// jittered backoff (and knowing from the server's close code when not to),
// resubscribing from the next seq each topic expects so nothing still in
// the log is missed, resending confirmed publishes under the same request
// id and message_id so the server stores them once, acknowledging QoS 1
// messages, and handing each (topic, seq) to the application at most once.
//
// A durable session outlives the process. When a new process connects with
// the same SessionID, the session's topics exist on the server before the
// application has subscribed to anything. The client asks the server to
// hold them (hold_resumed in auth): a server that does sends nothing on a
// session topic until the application subscribes to it, by name or through
// a pattern that matches it, and then continues one past the session's
// cursor. After a reconnect the client subscribes again to the session
// topics it tracks, since the server no longer attaches them.
//
// A server that does not know the request starts sending on the session's
// topics straight away. The client then leaves those messages
// unacknowledged, so the server keeps them, and they are replayed once the
// application subscribes. Messages on a session topic the application
// never subscribes to stay on the server.
//
// Example:
//
//	cfg := popsub.Config{
//		URL:         "ws://127.0.0.1:8080",
//		Credentials: popsub.PasswordCredentials("alice", "secret-password"),
//		SessionID:   "alice-laptop",
//	}
//	client, err := popsub.Connect(ctx, cfg)
//	if err != nil {
//		return err
//	}
//	defer client.Close()
//
//	orders, err := client.Subscribe(ctx, "orders", popsub.FromResume)
//	if err != nil {
//		return err
//	}
//	stored, err := client.PublishConfirmed(ctx, "orders", "order 42 shipped")
//	if err != nil {
//		return err
//	}
//	fmt.Println("stored as seq", stored.Seq)
//	for message := range orders.Messages() {
//		fmt.Printf("%s #%d: %s\n", message.Topic, message.Seq, message.Payload)
//	}
//
// A payload is a string. For a body that isn't text, PublishBytes and
// PublishBytesConfirmed send the bytes as standard padded base64 and mark
// the message with EncodingBase64; the server carries the string and the
// mark without decoding. Message.Bytes returns a message's bytes either
// way: the decoded payload for such a message, and the payload's UTF-8 for
// text. Base64 is about a third larger than the bytes, and it is the
// base64 string that counts against the server's payload limit.
//
//	if _, err := client.PublishBytesConfirmed(ctx, "blobs", []byte{0x00, 0xff}); err != nil {
//		return err
//	}
//	for message := range blobs.Messages() {
//		body, err := message.Bytes()
//		if err != nil {
//			continue
//		}
//		fmt.Printf("%s #%d: %d bytes\n", message.Topic, message.Seq, len(body))
//	}
//
// The protocol is described in the PopSub repository's docs/DESIGN.md
// section 4, and the behaviour every PopSub client shares in clients/README.md.
package popsub
