package main

import (
	"context"
	"fmt"
	"log"

	popsub "github.com/soumyadepp/popsub/clients/go"
)

func main() {
	ctx := context.Background()
	client, err := popsub.Connect(ctx, popsub.Config{
		URL:         "ws://127.0.0.1:8080",
		Credentials: popsub.PasswordCredentials("alice", "alice-password"),
		SessionID:   "orders-worker",
	})
	if err != nil {
		log.Fatal(err)
	}
	defer client.Close()

	messages, err := client.Subscribe(ctx, "orders/#", popsub.FromSeq(1))
	if err != nil {
		log.Fatal(err)
	}
	if _, err := client.PublishConfirmed(ctx, "orders/42", "order 42 shipped"); err != nil {
		log.Fatal(err)
	}
	for message := range messages.Messages() {
		fmt.Printf("%s #%d: %s\n", message.Topic, message.Seq, message.Payload)
	}
}
