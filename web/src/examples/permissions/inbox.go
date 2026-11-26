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
	username := os.Args[1]
	ctx := context.Background()

	client, err := popsub.Connect(ctx, popsub.Config{
		URL:         "ws://127.0.0.1:8080",
		Credentials: popsub.PasswordCredentials(username, username+"-password"),
	})
	if err != nil {
		log.Fatal(err)
	}
	defer client.Close()

	_, err = client.PublishConfirmed(ctx, "inbox/"+username+"/notes", "hello")
	var refused *popsub.Error
	if errors.As(err, &refused) && refused.Kind == popsub.KindServer {
		fmt.Println("publish refused:", refused.Code)
	} else if err != nil {
		log.Fatal(err)
	}

	inbox, err := client.Subscribe(ctx, "inbox/"+username+"/#", popsub.FromSeq(1))
	if err != nil {
		log.Fatal(err)
	}
	for message := range inbox.Messages() {
		fmt.Printf("%s: %s\n", message.Topic, message.Payload)
	}
}
