package main

import (
	"context"
	"fmt"
	"log"
	"os"
	"strconv"

	popsub "github.com/soumyadepp/popsub/clients/go"
)

func main() {
	count, err := strconv.Atoi(os.Args[1])
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

	for k := 1; k <= count; k++ {
		topic := fmt.Sprintf("invoices/%d", k)
		stored, err := client.PublishConfirmed(ctx, topic, fmt.Sprintf("invoice %d", k))
		if err != nil {
			log.Fatal(err)
		}
		fmt.Printf("queued %s as #%d\n", topic, stored.Seq)
	}
}
