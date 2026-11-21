// Command publish publishes one message and waits until the server has
// stored it.
//
//	go run ./examples/publish orders/go 'hello from Go'
//
// POPSUB_URL defaults to ws://127.0.0.1:8080, POPSUB_USERNAME to alice and
// POPSUB_USER_PASSWORD to alice-password. The topic defaults to orders/go
// and the message to "hello from Go".
package main

import (
	"context"
	"fmt"
	"log"
	"os"

	popsub "github.com/soumyadepp/popsub/clients/go"
)

func setting(name, fallback string) string {
	if value, ok := os.LookupEnv(name); ok {
		return value
	}
	return fallback
}

func main() {
	topic, payload := "orders/go", "hello from Go"
	if len(os.Args) > 1 {
		topic = os.Args[1]
	}
	if len(os.Args) > 2 {
		payload = os.Args[2]
	}
	ctx := context.Background()

	client, err := popsub.Connect(ctx, popsub.Config{
		URL:         setting("POPSUB_URL", "ws://127.0.0.1:8080"),
		Credentials: popsub.PasswordCredentials(setting("POPSUB_USERNAME", "alice"), setting("POPSUB_USER_PASSWORD", "alice-password")),
	})
	if err != nil {
		log.Fatal(err)
	}
	defer client.Close()

	stored, err := client.PublishConfirmed(ctx, topic, payload)
	if err != nil {
		log.Fatal(err)
	}
	fmt.Printf("stored on %s as seq %d (%s)\n", topic, stored.Seq, stored.MessageID)
}
