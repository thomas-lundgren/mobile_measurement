//! Wire enums — hand-written, with exact `serde(rename)` wire strings.
//! Parity-tested against `contracts/enums/enums.yaml`.

/// Declares a wire enum with explicit serialized strings plus an `ALL` slice
/// and `wire()` accessor used by the parity test.
macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        $name:ident { $( $variant:ident => $wire:literal ),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash,
            serde::Serialize, serde::Deserialize
        )]
        pub enum $name {
            $( #[serde(rename = $wire)] $variant ),+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [$name] = &[ $( $name::$variant ),+ ];

            /// The exact wire string for this variant.
            pub fn wire(self) -> &'static str {
                match self { $( $name::$variant => $wire ),+ }
            }
        }

        // Hand-written OpenAPI schema. The `#[derive(ToSchema)]` would emit the
        // Rust *variant* identifiers, ignoring `#[serde(rename)]` — so generated
        // clients couldn't (de)serialize the actual lowercase/SCREAMING wire
        // values. Emit the real wire strings (the same ones `wire()` returns).
        impl utoipa::PartialSchema for $name {
            fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
                utoipa::openapi::RefOr::T(utoipa::openapi::schema::Schema::Object(
                    utoipa::openapi::schema::ObjectBuilder::new()
                        .schema_type(utoipa::openapi::schema::Type::String)
                        .enum_values(Some([ $( $wire ),+ ]))
                        .build(),
                ))
            }
        }
        impl utoipa::ToSchema for $name {}
    };
}

wire_enum!(
    /// How an app operation's bytes move (`enums.yaml` app_engines).
    AppEngine {
        Exchange => "exchange",
        Bulk     => "bulk",
    }
);

wire_enum!(
    /// Server → device commands (`enums.yaml` commands).
    Command {
        RunTest          => "RUN_TEST",
        StopTest         => "STOP_TEST",
        SetLoggingConfig => "SET_LOGGING_CONFIG",
        RequestStatus    => "REQUEST_STATUS",
        RequestUpload    => "REQUEST_UPLOAD",
        RebootLogger     => "REBOOT_LOGGER",
        ListApps         => "LIST_APPS",
    }
);

wire_enum!(
    /// test-control per-device state (`enums.yaml` test_control_states).
    TestControlState {
        Idle                => "IDLE",
        Prepare             => "PREPARE",
        WaitingForGenerator => "WAITING_FOR_GENERATOR",
        WaitingForClient    => "WAITING_FOR_CLIENT",
        Running             => "RUNNING",
        Stopping            => "STOPPING",
        Done                => "DONE",
        Failed              => "FAILED",
    }
);

wire_enum!(
    /// data-generator state (`enums.yaml` generator_states).
    GeneratorState {
        Initializing     => "INITIALIZING",
        Ready            => "READY",
        WaitingForClient => "WAITING_FOR_CLIENT",
        Running          => "RUNNING",
        Stopping         => "STOPPING",
        Stopped          => "STOPPED",
        Failed           => "FAILED",
    }
);

wire_enum!(
    /// Logger app state (`enums.yaml` logger_states).
    LoggerState {
        Initializing   => "INITIALIZING",
        Registering    => "REGISTERING",
        Idle           => "IDLE",
        WaitingForTest => "WAITING_FOR_TEST",
        Preparing      => "PREPARING",
        RunningTest    => "RUNNING_TEST",
        Cooldown       => "COOLDOWN",
        UploadPending  => "UPLOAD_PENDING",
        Uploading      => "UPLOADING",
        Error          => "ERROR",
    }
);

wire_enum!(
    /// Flow type (`enums.yaml` flow_types).
    FlowType {
        Probe       => "probe",
        RealtimeUdp => "realtime_udp",
        BulkTcp     => "bulk_tcp",
        GatewayPing => "gateway_ping",
        DscpProbe   => "dscp_probe",
        AppTraffic  => "app_traffic",
    }
);

wire_enum!(
    /// Flow direction (`enums.yaml` directions). Bidirectional intentionally absent.
    Direction {
        FromServer => "FromServer",
        ToServer   => "ToServer",
    }
);

wire_enum!(
    /// Node role within a flow (`enums.yaml` roles).
    Role {
        Mobile => "mobile",
        Server => "server",
    }
);

wire_enum!(
    /// Operator JWT role claim (`enums.yaml` operator_roles).
    OperatorRole {
        Viewer   => "viewer",
        Operator => "operator",
        Admin    => "admin",
    }
);

wire_enum!(
    /// Device platform (`enums.yaml` platforms).
    Platform {
        Android => "android",
        Ios     => "ios",
    }
);

wire_enum!(
    /// events.parquet event_type (`enums.yaml` event_types).
    EventType {
        TestStart            => "TEST_START",
        TestStop             => "TEST_STOP",
        SignalLossStart      => "SIGNAL_LOSS_START",
        SignalLossEnd        => "SIGNAL_LOSS_END",
        ClientConnected      => "CLIENT_CONNECTED",
        ServerDisconnected   => "SERVER_DISCONNECTED",
        UploadStarted        => "UPLOAD_STARTED",
        Error                => "ERROR",
        FirstHopChange       => "FIRST_HOP_CHANGE",
        MonitorConfigApplied => "MONITOR_CONFIG_APPLIED",
        VpnScopeChange       => "VPN_SCOPE_CHANGE",
        QuotaReached         => "QUOTA_REACHED",
        Handover             => "HANDOVER",
        NeighborCountChange  => "NEIGHBOR_COUNT_CHANGE",
        CoverageGap          => "COVERAGE_GAP",
        BandChange           => "BAND_CHANGE",
    }
);

wire_enum!(
    /// Terminal test result (`enums.yaml` test_results).
    TestResult {
        Completed => "COMPLETED",
        Partial   => "PARTIAL",
        Failed    => "FAILED",
        Cancelled => "CANCELLED",
    }
);

wire_enum!(
    /// Enrolled app role (`enums.yaml` device_roles).
    DeviceRole {
        Logger  => "logger",
        Monitor => "monitor",
    }
);

wire_enum!(
    /// VPN capture scope (`enums.yaml` vpn_scopes).
    VpnScope {
        All       => "all",
        Allowlist => "allowlist",
    }
);

wire_enum!(
    /// Ping probe transport kind (`enums.yaml` probe_kinds).
    ProbeKind {
        Icmp        => "icmp",
        Udp         => "udp",
        TcpSyn      => "tcp_syn",
        Unreachable => "unreachable",
    }
);
