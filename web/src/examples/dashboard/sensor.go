package main

import (
	"context"
	"fmt"
	"log"
	"os"
	"time"

	popsub "github.com/soumyadepp/popsub/clients/go"
)

func main() {
	room := os.Args[1]
	ctx := context.Background()

	client, err := popsub.Connect(ctx, popsub.Config{
		URL:         "ws://127.0.0.1:8080",
		Credentials: popsub.PasswordCredentials("alice", "alice-password"),
	})
	if err != nil {
		log.Fatal(err)
	}
	defer client.Close()

	for temp := 20.0; ; temp += 0.1 {
		if err := client.Publish("sensors/"+room+"/temp", fmt.Sprintf("%.1f", temp)); err != nil {
			log.Fatal(err)
		}
		time.Sleep(time.Second)
	}
}
