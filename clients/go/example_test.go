package popsub_test

import (
	"context"
	"errors"
	"fmt"
	"time"

	popsub "github.com/soumyadepp/popsub/clients/go"
)

// Example connects with a durable session, subscribes, publishes a
// confirmed message and reads the topic until the subscription ends.
func Example() {
	ctx, cancel := context.WithTimeout(context.Background(), time.Minute)
	defer cancel()
	client, err := popsub.Connect(ctx, popsub.Config{
		URL:         "ws://127.0.0.1:8080",
		Credentials: popsub.PasswordCredentials("alice", "secret-password"),
		SessionID:   "alice-laptop",
		AckMode:     popsub.AckManual,
	})
	if err != nil {
		var e *popsub.Error
		if errors.As(err, &e) && e.Code == popsub.CodeInvalidCredentials {
			fmt.Println("wrong username or password")
		}
		return
	}
	defer client.Close()

	orders, err := client.Subscribe(ctx, "orders", popsub.FromResume)
	if err != nil {
		return
	}
	stored, err := client.PublishConfirmed(ctx, "orders", "order 42 shipped")
	if err != nil {
		return
	}
	fmt.Println("stored as seq", stored.Seq)
	for message := range orders.Messages() {
		fmt.Printf("%s #%d: %s\n", message.Topic, message.Seq, message.Payload)
		message.Ack()
	}
}
