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
		Credentials: popsub.PasswordCredentials("admin", "password"),
	})
	if err != nil {
		log.Fatal(err)
	}
	defer client.Close()

	_, err = client.Admin().CreateUser(ctx, username, username+"-password", popsub.Role{
		Kind:   popsub.RoleUser,
		Topics: []popsub.TopicGrant{{Pattern: "inbox/" + username + "/#", Access: popsub.AccessSubscribe}},
	})

	var refused *popsub.Error
	switch {
	case errors.As(err, &refused) && refused.Code == popsub.CodeUserExists:
		fmt.Println(username, "already exists")
	case err != nil:
		log.Fatal(err)
	default:
		fmt.Printf("created %s, may read inbox/%s/#\n", username, username)
	}
}
