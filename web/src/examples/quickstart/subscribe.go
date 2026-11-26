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
	})
	if err != nil {
		log.Fatal(err)
	}
	defer client.Close()

	greetings, err := client.Subscribe(ctx, "greetings", popsub.FromResume)
	if err != nil {
		log.Fatal(err)
	}
	fmt.Println("waiting for messages on greetings")
	for message := range greetings.Messages() {
		fmt.Printf("%s #%d: %s\n", message.Topic, message.Seq, message.Payload)
	}
}
