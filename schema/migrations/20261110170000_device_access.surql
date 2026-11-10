-- Migration: device_access
-- Created: 2026-11-10T17:00:00+00:00
-- Written by hand, the same definition as schema.surql.
-- The desktop app's sync key signs in to SurrealDB (schema.surql, ACCESS).

DEFINE ACCESS IF NOT EXISTS device ON DATABASE TYPE RECORD
  SIGNIN (
    SELECT * FROM user WHERE id = (
      SELECT VALUE owner FROM sync_device WHERE token_hash = crypto::sha256($key)
        AND connection.id != NONE AND connection.revoked_at = NONE AND connection.owner = owner LIMIT 1
    )[0]
  )
  DURATION FOR TOKEN 1d, FOR SESSION 1d
;
