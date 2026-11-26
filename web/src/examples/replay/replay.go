package main

import (
	"context"
	"fmt"
	"log"
	"os"
	"strconv"
	"time"

	popsub "github.com/soumyadepp/popsub/clients/go"
)

func main() {
	minutes, err := strconv.Atoi(os.Args[1])
	if err != nil {
		log.Fatal(err)
	}
	ctx := context.Background()

	client, err := popsub.Connect(ctx, popsub.Config{
		URL:         "ws://127.0.0.1:8080",
		Credentials: popsub.PasswordCredentials("alice", "alice-password"),
	})
	if err != nil {
		log.Fatal(err)
	}
	defer client.Close()

	since := time.Now().Add(-time.Duration(minutes) * time.Minute)
	audit, err := client.Subscribe(ctx, "audit/#", popsub.FromSince(since))
	if err != nil {
		log.Fatal(err)
	}
	for message := range audit.Messages() {
		seconds := (time.Now().UnixMilli() - message.PublishedAtMs) / 1000
		fmt.Printf("%d s ago  %s  %s\n", seconds, message.Topic, message.Payload)
	}
}
