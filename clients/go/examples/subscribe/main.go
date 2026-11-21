// Command subscribe subscribes to a topic pattern with a durable session and
// prints every message.
//
// Stop it and run it again: messages published while it was away arrive once
// it is back, each once, because the session keeps its place.
//
//	go run ./examples/subscribe 'orders/#'
//
// POPSUB_URL defaults to ws://127.0.0.1:8080, POPSUB_USERNAME to alice and
// POPSUB_USER_PASSWORD to alice-password; the pattern defaults to orders/#.
package main

import (
	"context"
	"fmt"
	"log"
	"os"
	"os/signal"

	popsub "github.com/soumyadepp/popsub/clients/go"
)

func setting(name, fallback string) string {
	if value, ok := os.LookupEnv(name); ok {
		return value
	}
	return fallback
}

func main() {
	pattern := "orders/#"
	if len(os.Args) > 1 {
		pattern = os.Args[1]
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt)
	defer stop()

	client, err := popsub.Connect(ctx, popsub.Config{
		URL:         setting("POPSUB_URL", "ws://127.0.0.1:8080"),
		Credentials: popsub.PasswordCredentials(setting("POPSUB_USERNAME", "alice"), setting("POPSUB_USER_PASSWORD", "alice-password")),
		SessionID:   "examples-subscriber",
	})
	if err != nil {
		log.Fatal(err)
	}
	defer client.Close()

	messages, err := client.Subscribe(ctx, pattern, popsub.FromResume)
	if err != nil {
		log.Fatal(err)
	}
	fmt.Printf("subscribed to %s; waiting for messages (Ctrl+C to stop)\n", pattern)
	for {
		select {
		case <-ctx.Done():
			return
		case message, ok := <-messages.Messages():
			if !ok {
				return
			}
			fmt.Printf("%s #%d: %s\n", message.Topic, message.Seq, message.Payload)
		}
	}
}
