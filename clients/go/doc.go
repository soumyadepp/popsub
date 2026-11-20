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
// the same SessionID, the server starts sending what it kept for the
// session's topics before the application has subscribed. The client
// leaves those messages unacknowledged, so the server keeps them, and they
// are replayed once the application subscribes to the topic by name or
// through a pattern that matches it. Messages on a session topic the
// application never subscribes to stay on the server.
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
// The protocol is described in the PopSub repository's docs/DESIGN.md
// section 4, and the behaviour every PopSub client shares in clients/README.md.
package popsub
