package main

import (
	"bufio"
	"context"
	"fmt"
	"log"
	"os"

	popsub "github.com/soumyadepp/popsub/clients/go"
)

func main() {
	name := os.Args[1]
	ctx := context.Background()

	client, err := popsub.Connect(ctx, popsub.Config{
		URL:         "ws://127.0.0.1:8080",
		Credentials: popsub.PasswordCredentials("alice", "alice-password"),
	})
	if err != nil {
		log.Fatal(err)
	}
	defer client.Close()

	lobby, err := client.Subscribe(ctx, "chat/lobby", popsub.FromLast(20))
	if err != nil {
		log.Fatal(err)
	}
	go func() {
		for message := range lobby.Messages() {
			fmt.Println(message.Payload)
		}
	}()

	input := bufio.NewScanner(os.Stdin)
	for input.Scan() {
		if input.Text() == "" {
			continue
		}
		if err := client.Publish("chat/lobby", name+": "+input.Text()); err != nil {
			log.Fatal(err)
		}
	}
}
