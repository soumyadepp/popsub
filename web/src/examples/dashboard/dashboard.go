package main

import (
	"context"
	"fmt"
	"log"
	"maps"
	"slices"
	"strings"

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

	temps, err := client.Subscribe(ctx, "sensors/+/temp", popsub.FromLast(1))
	if err != nil {
		log.Fatal(err)
	}

	latest := map[string]string{}
	for message := range temps.Messages() {
		room := strings.Split(message.Topic, "/")[1]
		latest[room] = message.Payload

		var readings []string
		for _, room := range slices.Sorted(maps.Keys(latest)) {
			readings = append(readings, room+" "+latest[room])
		}
		fmt.Println(strings.Join(readings, "  "))
	}
}
