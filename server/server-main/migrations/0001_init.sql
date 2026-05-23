-- server-main database schema (resolves remediation 3.3).
--
-- SQLite-first (WAL mode). Control-plane METADATA only — no measurement data:
-- packets/radio/gps/clock/events live as Parquet on the filesystem and are
-- referenced by path, never stored here.
--
-- Enum CHECK constraints mirror contracts/enums/enums.yaml. Timestamps are
-- INTEGER nanoseconds (UTC ns, i64) unless named *_mono_ns. Booleans are
-- INTEGER 0/1.
--
-- Postgres portability notes (if the fleet outgrows one host):
--   * "INTEGER PRIMARY KEY" autoincrement -> "BIGSERIAL/IDENTITY".
--   * SQLite has dynamic typing; Postgres will enforce types strictly.
--   * JSON is stored as TEXT here; Postgres could use JSONB.
-- The table/column shapes and CHECK sets are dialect-neutral.

-- ---------------------------------------------------------------------------
-- Auth
-- ---------------------------------------------------------------------------

-- Operators (Controller users). Auth: username/password -> JWT.
CREATE TABLE operators (
  id            TEXT PRIMARY KEY,                 -- opr_<ULID>
  username      TEXT NOT NULL UNIQUE,
  password_hash TEXT NOT NULL,                    -- argon2/bcrypt
  role          TEXT NOT NULL CHECK (role IN ('viewer','operator','admin')),
  created_at    INTEGER NOT NULL
);

-- One-time device enrollment tokens (operator-issued, out of band).
CREATE TABLE enrollment_tokens (
  token_hash           TEXT PRIMARY KEY,          -- hash of the token, never the token
  created_at           INTEGER NOT NULL,
  expires_at           INTEGER NOT NULL,
  consumed_at          INTEGER,                   -- NULL until used; burned on enroll
  consumed_by_device_id TEXT REFERENCES devices(device_id)
);

-- Per-session challenge nonces (short TTL, single-use).
CREATE TABLE auth_challenges (
  challenge_id TEXT PRIMARY KEY,                  -- chl_<ULID>
  device_id    TEXT NOT NULL REFERENCES devices(device_id),
  nonce        TEXT NOT NULL,                     -- base64, 32 random bytes
  server_id    TEXT NOT NULL,                     -- bound into the signed blob
  created_at   INTEGER NOT NULL,
  expires_at   INTEGER NOT NULL,
  consumed_at  INTEGER                            -- NULL until verified
);

-- Opaque device session tokens issued by /auth/verify.
CREATE TABLE device_sessions (
  session_token_hash TEXT PRIMARY KEY,            -- hash of the opaque token
  device_id          TEXT NOT NULL REFERENCES devices(device_id),
  issued_at          INTEGER NOT NULL,
  expires_at         INTEGER NOT NULL,
  revoked_at         INTEGER                       -- NULL unless revoked
);
CREATE INDEX idx_device_sessions_device ON device_sessions(device_id);

-- ---------------------------------------------------------------------------
-- Registry
-- ---------------------------------------------------------------------------

-- Enrolled devices. pubkey is the credential (challenge/response); deleting a
-- row revokes the device.
CREATE TABLE devices (
  device_id      TEXT PRIMARY KEY,                -- dev_<ULID>, server-assigned
  node_id        TEXT NOT NULL,
  device_label   TEXT,
  pubkey_spki    TEXT NOT NULL,                   -- SPKI DER, base64
  pubkey_format  TEXT NOT NULL DEFAULT 'SPKI_DER' CHECK (pubkey_format IN ('SPKI_DER')),
  sig_alg        TEXT NOT NULL DEFAULT 'ECDSA_P256_SHA256' CHECK (sig_alg IN ('ECDSA_P256_SHA256')),
  platform       TEXT NOT NULL CHECK (platform IN ('android','ios')),
  model          TEXT,
  manufacturer   TEXT,
  os_version     TEXT,
  capability_json TEXT,                           -- latest reported Capability (capability.schema.json)
  logging_config_version INTEGER NOT NULL DEFAULT 1,
  status         TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','disabled')),
  enrolled_at    INTEGER NOT NULL,
  created_at     INTEGER NOT NULL
);

-- Latest heartbeat per device (device_status.schema.json). Upserted on each
-- POST /status; one row per device (history is in the Parquet logs, not here).
CREATE TABLE device_status (
  device_id              TEXT PRIMARY KEY REFERENCES devices(device_id),
  logger_state           TEXT NOT NULL CHECK (logger_state IN
                           ('INITIALIZING','REGISTERING','IDLE','WAITING_FOR_TEST',
                            'PREPARING','RUNNING_TEST','COOLDOWN','UPLOAD_PENDING',
                            'UPLOADING','ERROR')),
  current_test_id        TEXT,  -- informational (dashboard); intentionally no FK so a heartbeat never 500s on test-row timing
  logging_config_version INTEGER NOT NULL,
  battery_level_pct      INTEGER CHECK (battery_level_pct BETWEEN 0 AND 100),
  battery_charging       INTEGER CHECK (battery_charging IN (0,1)),
  storage_free_bytes     INTEGER,
  storage_total_bytes    INTEGER,
  pending_uploads        INTEGER,
  radio_json             TEXT,                    -- coarse serving-cell snapshot
  sampled_mono_ns        INTEGER NOT NULL,
  sampled_utc_ns         INTEGER NOT NULL,
  received_at            INTEGER NOT NULL
);

-- ---------------------------------------------------------------------------
-- Orchestration
-- ---------------------------------------------------------------------------

-- Logging sessions: group tests recorded together (the session_<date> dir).
CREATE TABLE sessions (
  session_id TEXT PRIMARY KEY,                    -- ses_...
  device_id  TEXT NOT NULL REFERENCES devices(device_id),
  started_at INTEGER NOT NULL,
  ended_at   INTEGER
);
CREATE INDEX idx_sessions_device ON sessions(device_id);

-- Tests. plan_json is the port-less TestPlan; flows (with bindings) are
-- normalized into the flows table for queryability.
CREATE TABLE tests (
  test_id        TEXT PRIMARY KEY,               -- tst_<ULID>
  session_id     TEXT NOT NULL REFERENCES sessions(session_id),
  device_id      TEXT NOT NULL REFERENCES devices(device_id),
  plan_name      TEXT NOT NULL,
  plan_version   INTEGER NOT NULL,
  plan_json      TEXT NOT NULL,                  -- TestPlan (test_plan.schema.json)
  server_host    TEXT NOT NULL,
  logging_config_json    TEXT NOT NULL,          -- effective LoggingConfig used
  logging_config_version INTEGER NOT NULL,
  state          TEXT NOT NULL CHECK (state IN
                   ('IDLE','PREPARE','WAITING_FOR_GENERATOR','WAITING_FOR_CLIENT',
                    'RUNNING','STOPPING','DONE','FAILED')),
  result         TEXT CHECK (result IN ('COMPLETED','PARTIAL','FAILED','CANCELLED')),
  result_reason  TEXT,
  created_at     INTEGER NOT NULL,
  started_at     INTEGER,
  ended_at       INTEGER
);
CREATE INDEX idx_tests_device_state ON tests(device_id, state);
CREATE INDEX idx_tests_session ON tests(session_id);

-- Per-flow config + the server-allocated binding (flow_bindings).
CREATE TABLE flows (
  test_id           TEXT NOT NULL REFERENCES tests(test_id),
  flow_id           INTEGER NOT NULL,
  type              TEXT NOT NULL CHECK (type IN ('probe','realtime_udp','bulk_tcp')),
  direction         TEXT CHECK (direction IN ('FromServer','ToServer')),  -- NULL for probe
  server_port       INTEGER NOT NULL,            -- allocated binding
  rate_pps          INTEGER,
  packet_size_bytes INTEGER,
  target_bytes      INTEGER,
  PRIMARY KEY (test_id, flow_id)
);

-- Device command queue. The device polls GET /command -> oldest pending.
CREATE TABLE commands (
  id           INTEGER PRIMARY KEY,              -- autoincrement rowid
  device_id    TEXT NOT NULL REFERENCES devices(device_id),
  command      TEXT NOT NULL CHECK (command IN
                 ('RUN_TEST','STOP_TEST','SET_LOGGING_CONFIG','REQUEST_STATUS',
                  'REQUEST_UPLOAD','REBOOT_LOGGER')),
  payload_json TEXT,                             -- e.g. RunTestPayload for RUN_TEST
  status       TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','delivered','acked')),
  created_at   INTEGER NOT NULL,
  delivered_at INTEGER,
  acked_at     INTEGER
);
CREATE INDEX idx_commands_device_status ON commands(device_id, status, id);

-- ---------------------------------------------------------------------------
-- Upload
-- ---------------------------------------------------------------------------

-- One upload session per (test, node). node is 'mobile' (the server is the
-- destination and never uploads).
CREATE TABLE uploads (
  upload_id    TEXT PRIMARY KEY,                 -- upl_<ULID>
  test_id      TEXT NOT NULL REFERENCES tests(test_id),
  node         TEXT NOT NULL CHECK (node IN ('mobile')),
  state        TEXT NOT NULL DEFAULT 'PENDING' CHECK (state IN
                 ('PENDING','IN_PROGRESS','COMPLETE','FAILED')),
  test_result  TEXT CHECK (test_result IN ('COMPLETED','PARTIAL','FAILED','CANCELLED')),
  manifest_json TEXT,  -- the expected file set (paths + sha256); checked at complete
  created_at   INTEGER NOT NULL,
  completed_at INTEGER,
  UNIQUE (test_id, node)
);

-- Received files, keyed by (test,node,path). Drives needed_paths: a manifest
-- file already present here with a matching sha256 is omitted from the next
-- needed_paths response (resume).
CREATE TABLE upload_files (
  test_id     TEXT NOT NULL REFERENCES tests(test_id),
  node        TEXT NOT NULL CHECK (node IN ('mobile')),
  path        TEXT NOT NULL,                     -- test-relative, e.g. mobile/packets.parquet
  sha256      TEXT NOT NULL,
  size_bytes  INTEGER NOT NULL,
  encoding    TEXT NOT NULL CHECK (encoding IN ('identity','zstd')),
  received_at INTEGER NOT NULL,
  PRIMARY KEY (test_id, node, path)
);
