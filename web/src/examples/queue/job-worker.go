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
	name := os.Args[1]
	ctx := context.Background()

	client, err := popsub.Connect(ctx, popsub.Config{
		URL:         "ws://127.0.0.1:8080",
		Credentials: popsub.PasswordCredentials("alice", "alice-password"),
		AckMode:     popsub.AckManual,
	})
	if err != nil {
		log.Fatal(err)
	}
	defer client.Close()

	jobs, err := client.SubscribeShared(ctx, "jobs/resize", "resizers", popsub.FromNew)
	if err != nil {
		log.Fatal(err)
	}
	for job := range jobs.Messages() {
		fmt.Println(name, "took", job.Payload)
		time.Sleep(time.Second)
		fmt.Println(name, "finished", job.Payload)
		job.Ack()
	}
}
