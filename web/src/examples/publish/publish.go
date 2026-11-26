package main

import (
	"context"
	"errors"
	"fmt"
	"log"
	"os"

	popsub "github.com/soumyadepp/popsub/clients/go"
)

func main() {
	topic, text := os.Args[1], os.Args[2]
	ctx := context.Background()

	client, err := popsub.Connect(ctx, popsub.Config{
		URL:         "ws://127.0.0.1:8080",
		Credentials: popsub.PasswordCredentials("alice", "alice-password"),
	})
	if err != nil {
		log.Fatal(err)
	}

	stored, err := client.PublishConfirmed(ctx, topic, text)
	client.Close()

	var refused *popsub.Error
	if errors.As(err, &refused) && refused.Kind == popsub.KindServer {
		fmt.Fprintln(os.Stderr, "refused:", refused.Code)
		os.Exit(1)
	}
	if err != nil {
		log.Fatal(err)
	}
	fmt.Printf("stored on %s as #%d\n", topic, stored.Seq)
}
