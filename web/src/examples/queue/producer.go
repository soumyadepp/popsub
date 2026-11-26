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

	for k := 1; k <= 6; k++ {
		job := fmt.Sprintf("job-%d", k)
		if _, err := client.PublishConfirmed(ctx, "jobs/resize", job); err != nil {
			log.Fatal(err)
		}
		fmt.Println("queued", job)
	}
}
