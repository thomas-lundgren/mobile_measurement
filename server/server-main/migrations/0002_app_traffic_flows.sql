-- app_traffic flows (fd0a60d) were refused by the flows.type CHECK, so every
-- test with one failed at creation. SQLite cannot alter a CHECK: rebuild the
-- table with the type added. Nothing references flows.
CREATE TABLE flows_new (
  test_id           TEXT NOT NULL REFERENCES tests(test_id),
  flow_id           INTEGER NOT NULL,
  type              TEXT NOT NULL CHECK (type IN ('probe','realtime_udp','bulk_tcp','gateway_ping','dscp_probe','app_traffic')),
  direction         TEXT CHECK (direction IN ('FromServer','ToServer')),  -- NULL for probe/gateway_ping/dscp_probe/app_traffic
  server_port       INTEGER,                     -- allocated binding (NULL for gateway_ping)
  rate_pps          INTEGER,
  packet_size_bytes INTEGER,
  target_bytes      INTEGER,
  PRIMARY KEY (test_id, flow_id)
);
INSERT INTO flows_new SELECT * FROM flows;
DROP TABLE flows;
ALTER TABLE flows_new RENAME TO flows;
