package popsub

import (
	"context"
	"encoding/json"
)

// Admin sends the account management requests: creating accounts,
// granting and revoking topics, disabling and deleting accounts. Get one
// with Client.Admin.
//
// Every call fails with KindServer and CodeForbidden unless the client is
// logged in as an enabled admin. None is queued while the client is
// disconnected or resent after a reconnect, because repeating a change
// such as CreateUser is not safe: a call made while disconnected fails
// with KindConnect, and so does one whose connection drops before the
// reply, whether or not the server carried it out. Calls also fail with
// KindTimeout or KindClosed.
type Admin struct {
	// client is the client the requests go through.
	client *Client
}

// CreateUser creates an account. It fails with CodeUserExists, or
// CodeBadRequest for an invalid name, password or pattern.
func (a *Admin) CreateUser(ctx context.Context, username, password string, role Role) (User, error) {
	return a.user(ctx, createUserRequest{Type: "create_user", Username: username, Password: password, Role: role})
}

// GetUser reads an account. It fails with CodeUserNotFound.
func (a *Admin) GetUser(ctx context.Context, username string) (User, error) {
	return a.user(ctx, usernameRequest{Type: "get_user", Username: username})
}

// ListUsers lists every account, sorted by name.
func (a *Admin) ListUsers(ctx context.Context) ([]User, error) {
	frame, err := a.client.request(ctx, listUsersRequest{Type: "list_users"})
	if err != nil {
		return nil, err
	}
	if frame.Type != "users" {
		return nil, protocolError("expected users, got " + frame.Type)
	}
	users := []User{}
	if err := json.Unmarshal(frame.Users, &users); err != nil {
		return nil, protocolError("the users frame doesn't parse: " + err.Error())
	}
	return users, nil
}

// SetRole replaces an account's role and its grants. Live subscriptions
// the new role does not allow are ended. It fails with CodeUserNotFound,
// or CodeBadRequest for an invalid pattern or for demoting the last
// enabled admin.
func (a *Admin) SetRole(ctx context.Context, username string, role Role) (User, error) {
	return a.user(ctx, setRoleRequest{Type: "set_role", Username: username, Role: role})
}

// Grant gives an account a topic pattern, replacing any grant it already
// has for exactly that pattern. It fails with CodeUserNotFound, or
// CodeBadRequest for an invalid pattern, an admin account, or publishing
// for a readonly one.
func (a *Admin) Grant(ctx context.Context, username, pattern string, access Access) (User, error) {
	return a.user(ctx, grantRequest{Type: "grant", Username: username, Pattern: pattern, Access: access})
}

// Revoke removes one of an account's grants, given exactly as granted.
// Live subscriptions that are no longer allowed are ended. It fails with
// CodeUserNotFound, or CodeBadRequest when the account has no such grant.
func (a *Admin) Revoke(ctx context.Context, username, pattern string) (User, error) {
	return a.user(ctx, revokeRequest{Type: "revoke", Username: username, Pattern: pattern})
}

// SetPassword sets an account's password. Tokens already issued stay
// valid until they expire. It fails with CodeUserNotFound, or
// CodeBadRequest for a password that is too short or too long.
func (a *Admin) SetPassword(ctx context.Context, username, password string) (User, error) {
	return a.user(ctx, setPasswordRequest{Type: "set_password", Username: username, Password: password})
}

// SetEnabled enables or disables an account. Disabling closes its
// connections with CloseAccountDisabled and refuses its logins and tokens.
// It fails with CodeUserNotFound, or CodeBadRequest for disabling the last
// enabled admin.
func (a *Admin) SetEnabled(ctx context.Context, username string, enabled bool) (User, error) {
	return a.user(ctx, setEnabledRequest{Type: "set_enabled", Username: username, Enabled: enabled})
}

// DeleteUser deletes an account and closes its connections with
// CloseAccountDisabled. It fails with CodeUserNotFound, or CodeBadRequest
// for the last enabled admin.
func (a *Admin) DeleteUser(ctx context.Context, username string) error {
	frame, err := a.client.request(ctx, usernameRequest{Type: "delete_user", Username: username})
	if err != nil {
		return err
	}
	if frame.Type != "user_deleted" {
		return protocolError("expected user_deleted, got " + frame.Type)
	}
	return nil
}

// user sends a request answered by a user frame and returns the account in it.
func (a *Admin) user(ctx context.Context, message any) (User, error) {
	frame, err := a.client.request(ctx, message)
	if err != nil {
		return User{}, err
	}
	if frame.Type != "user" {
		return User{}, protocolError("expected user, got " + frame.Type)
	}
	var user User
	if err := json.Unmarshal(frame.User, &user); err != nil {
		return User{}, protocolError("the user frame doesn't parse: " + err.Error())
	}
	return user, nil
}
