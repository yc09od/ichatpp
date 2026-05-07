-- Drop in reverse FK order: dependents first, then targets.
DROP INDEX IF EXISTS idx_invitations_expires_at;
DROP INDEX IF EXISTS idx_invitations_created_by;
DROP INDEX IF EXISTS idx_invitations_status;
DROP INDEX IF EXISTS idx_invitations_code_hash;
DROP INDEX IF EXISTS idx_friends_user2;
DROP INDEX IF EXISTS idx_friends_user1;
DROP INDEX IF EXISTS idx_messages_created_at;
DROP INDEX IF EXISTS idx_messages_conversation_reverse;
DROP INDEX IF EXISTS idx_messages_conversation;
DROP INDEX IF EXISTS idx_users_role;
DROP INDEX IF EXISTS idx_users_account_code;
DROP INDEX IF EXISTS uq_friend_requests_pending;

DROP TABLE IF EXISTS invitations;
DROP TABLE IF EXISTS user_roles;
DROP TABLE IF EXISTS message_emojis;
DROP TABLE IF EXISTS emojis;
DROP TABLE IF EXISTS messages;
DROP TABLE IF EXISTS friend_requests;
DROP TABLE IF EXISTS friends;
DROP TABLE IF EXISTS users;
