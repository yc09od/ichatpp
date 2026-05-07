-- ichatpp initial schema.
-- Mirrors ARCHITECTURE.md §5 verbatim. Order matters for FK references:
--   users → user_roles, friends, friend_requests, messages, emojis, invitations
--   emojis → message_emojis (FK on emoji_id)
--   messages → message_emojis (FK on message_id)

-- ────────── Users ──────────
-- Login credential is `email` only; `nickname` is the display name; the
-- legacy `username` field has been removed.
CREATE TABLE users (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  email VARCHAR(255) NOT NULL UNIQUE,
  password_hash VARCHAR(255) NOT NULL,
  account_code CHAR(10) NOT NULL UNIQUE CHECK (account_code ~ '^[0-9]{10}$'),
  nickname VARCHAR(100),
  avatar_url VARCHAR(255),
  signature TEXT,
  is_visible BOOLEAN DEFAULT TRUE,
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- ────────── Friends ──────────
-- `user_id_1 < user_id_2` keeps each pair canonical (one row per friendship).
CREATE TABLE friends (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id_1 UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  user_id_2 UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(user_id_1, user_id_2),
  CHECK (user_id_1 < user_id_2)
);

-- ────────── Friend requests ──────────
-- No table-level UNIQUE; we use a partial unique index further down so that
-- requests can be re-sent after being accepted/rejected, while preventing
-- multiple simultaneous pending requests between the same pair.
CREATE TABLE friend_requests (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  from_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  to_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  status VARCHAR(20) DEFAULT 'pending', -- pending, accepted, rejected
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX uq_friend_requests_pending
  ON friend_requests(from_user_id, to_user_id)
  WHERE status = 'pending';

-- ────────── Messages ──────────
CREATE TABLE messages (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  from_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  to_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  content TEXT NOT NULL,
  is_deleted BOOLEAN DEFAULT FALSE,
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- ────────── Emojis (must precede message_emojis for FK) ──────────
CREATE TABLE emojis (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name VARCHAR(100) NOT NULL,
  file_url VARCHAR(255) NOT NULL,
  thumbnail_url VARCHAR(255),
  mime_type VARCHAR(50),
  file_size INT,
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- ────────── Message ↔ emoji junction ──────────
CREATE TABLE message_emojis (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  message_id UUID NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
  emoji_id UUID NOT NULL REFERENCES emojis(id) ON DELETE CASCADE,
  position INT NOT NULL
);

-- ────────── User roles ──────────
CREATE TABLE user_roles (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id UUID NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
  role VARCHAR(50) DEFAULT 'user', -- user, admin
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- ────────── Invitations ──────────
-- DB stores only the SHA-256 hash; plaintext is returned exactly once at
-- generation time and cannot be recovered server-side afterwards.
CREATE TABLE invitations (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  code_hash CHAR(64) NOT NULL UNIQUE,    -- SHA-256(plaintext) hex (64 chars)
  code_prefix VARCHAR(8),                -- e.g. 'INV-AbCd'; lets admins identify the batch, never used for validation
  created_by UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  used_by UUID REFERENCES users(id) ON DELETE SET NULL,
  status VARCHAR(20) DEFAULT 'unused',   -- unused, used, expired, revoked
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  used_at TIMESTAMP,
  expires_at TIMESTAMP DEFAULT (CURRENT_TIMESTAMP + INTERVAL '7 days'),
  notes VARCHAR(255)
);

-- ────────── Indexes ──────────
CREATE INDEX idx_users_account_code ON users(account_code);
CREATE INDEX idx_users_role ON user_roles(user_id);

-- Conversation queries fetch by (sender, recipient, time DESC); we need both
-- directions because A→B and B→A are stored as separate rows.
CREATE INDEX idx_messages_conversation
  ON messages(from_user_id, to_user_id, created_at DESC);
CREATE INDEX idx_messages_conversation_reverse
  ON messages(to_user_id, from_user_id, created_at DESC);
CREATE INDEX idx_messages_created_at ON messages(created_at);

CREATE INDEX idx_friends_user1 ON friends(user_id_1);
CREATE INDEX idx_friends_user2 ON friends(user_id_2);

CREATE INDEX idx_invitations_code_hash ON invitations(code_hash);
CREATE INDEX idx_invitations_status ON invitations(status);
CREATE INDEX idx_invitations_created_by ON invitations(created_by);
CREATE INDEX idx_invitations_expires_at ON invitations(expires_at);
