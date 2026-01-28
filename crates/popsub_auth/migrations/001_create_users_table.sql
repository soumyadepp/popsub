-- PopSub Auth Users Table
-- Run this migration to create the users table for PostgreSQL storage

CREATE TABLE IF NOT EXISTS users (
    username VARCHAR(255) PRIMARY KEY,
    password_hash TEXT NOT NULL,
    role_type VARCHAR(50) NOT NULL,
    role_data JSONB,
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Indexes for common queries
CREATE INDEX IF NOT EXISTS idx_users_enabled ON users(enabled);
CREATE INDEX IF NOT EXISTS idx_users_role_type ON users(role_type);

-- Trigger to auto-update updated_at timestamp
CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ language 'plpgsql';

DROP TRIGGER IF EXISTS update_users_updated_at ON users;
CREATE TRIGGER update_users_updated_at
    BEFORE UPDATE ON users
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();

-- Example: Insert default admin user (password: 'password')
-- The hash below is for 'password' using Argon2id
-- You should generate a proper hash in production!
-- INSERT INTO users (username, password_hash, role_type, enabled)
-- VALUES ('admin', '$argon2id$v=19$m=19456,t=2,p=1$...', 'admin', true)
-- ON CONFLICT (username) DO NOTHING;

COMMENT ON TABLE users IS 'User accounts for PopSub authentication';
COMMENT ON COLUMN users.username IS 'Unique username for login';
COMMENT ON COLUMN users.password_hash IS 'Argon2id hashed password';
COMMENT ON COLUMN users.role_type IS 'User role: admin, user, or readonly';
COMMENT ON COLUMN users.role_data IS 'JSON data for role-specific settings (e.g., allowed topics)';
COMMENT ON COLUMN users.enabled IS 'Whether the user account is active';
