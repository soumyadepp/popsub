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
		SessionID:   "invoice-worker",
		AckMode:     popsub.AckManual,
	})
	if err != nil {
		log.Fatal(err)
	}
	defer client.Close()

	invoices, err := client.Subscribe(ctx, "invoices/#", popsub.FromSeq(1))
	if err != nil {
		log.Fatal(err)
	}
	for message := range invoices.Messages() {
		fmt.Printf("processing %s #%d: %s\n", message.Topic, message.Seq, message.Payload)
		message.Ack()
	}
}
