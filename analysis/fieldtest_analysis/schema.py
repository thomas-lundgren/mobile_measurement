"""Canonical column sets for each log table (mirrors ``contracts/parquet/*``).

Invariant 4 (capability-tolerant, forward-compatible logs): every field is
optional at the row level, new fields may appear without a schema bump, missing
fields read as null, and readers must tolerate unknown fields. The loader uses
these lists to *guarantee* the canonical columns exist (filling absent ones with
NA) while preserving any extra columns it doesn't recognise.
"""

from __future__ import annotations

# Columns carried as int64 nanosecond clocks (cast from uint64 on load so diffs
# are signed and safe). ns fits in int64 until year 2262.
TIME_COLUMNS = frozenset(
    {
        "mono_ns",
        "utc_ns",
        "sender_mono_ns",
        "sender_utc_ns",
        "expected_interval_ns",
        "actual_interval_ns",
        "kernel_rx_utc_ns",
        "uncertainty_ns",
        "estimated_offset_ns",
    }
)

PACKETS = [
    "device_id",
    "session_id",
    "test_id",
    "node",
    "flow_id",
    "sequence_number",
    "direction",
    "mono_ns",
    "utc_ns",
    "payload_size",
    "protocol",
    "local_port",
    "remote_port",
    "sender_mono_ns",
    "sender_utc_ns",
    "expected_interval_ns",
    "actual_interval_ns",
    "kernel_rx_utc_ns",
    "send_status",
    "send_call_ns",
    "sock_outq_bytes",
    "app_activity",
    "app_object_bytes",
    "app_up_bytes",
    "dscp_sent",
    "dscp_recv",
]

CLOCK = [
    "device_id",
    "session_id",
    "test_id",
    "node",
    "mono_ns",
    "utc_ns",
    "source",
    "uncertainty_ns",
    "estimated_offset_ns",
]

GPS = [
    "device_id",
    "session_id",
    "test_id",
    "mono_ns",
    "utc_ns",
    "latitude",
    "longitude",
    "altitude_m",
    "speed_mps",
    "heading_deg",
    "accuracy_m",
]

RADIO = [
    "device_id",
    "session_id",
    "test_id",
    "mono_ns",
    "utc_ns",
    "pci",
    "nci",
    "tac",
    "nrarfcn",
    "rsrp",
    "rsrq",
    "sinr",
    "earfcn",
    "lte_pci",
    "lte_rsrp",
    "bandwidth_mhz",
    "cell_connection_status",
    "network_type",
    "transport",
    "wifi_freq_mhz",
    "wifi_channel",
    "wifi_bw_mhz",
    "wifi_standard",
    "wifi_rssi",
    "wifi_tx_mbps",
    "wifi_rx_mbps",
    "lte_rsrq",
    "lte_rssnr",
    "lte_cqi",
    "lte_ta",
    "ca_bandwidths_khz",
    "link_dn_kbps",
    "link_up_kbps",
    "neighbor_cells",
]

EVENTS = [
    "device_id",
    "session_id",
    "test_id",
    "node",
    "mono_ns",
    "utc_ns",
    "event_type",
    "flow_id",
    "event_metadata",
]

# Sent/received discriminator and protocol/connection wire values.
DIR_SENT = "SENT"
DIR_RECEIVED = "RECEIVED"
PROTO_UDP = "UDP"
