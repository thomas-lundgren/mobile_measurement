//! `resolve_effective_config` (invariant 3): the device logs only what the
//! server enables, and only what it is capable of. At registration (no active
//! test) the effective config is the server default profile intersected with
//! the device's reported capability. Per-test-plan profiles + per-device
//! overrides layer on top later; the intersection-with-capability rule here is
//! the floor every result must satisfy: `effective ⊆ capability`.

use mp_contracts::{
    Capability, GateableEvent, LoggingConfig, MobilityLogging, QosLogging, RadioLogging,
    ServingCellField, TimestampSource,
};

/// Resolve the effective config for a device given its capability and the
/// config `version` to stamp.
pub fn resolve_effective_config(capability: &Capability, version: u32) -> LoggingConfig {
    let r = &capability.radio;
    // Default profile "wants everything"; intersect with capability.
    LoggingConfig {
        version,
        radio: RadioLogging {
            // serving_cell: keep only fields the device can report
            serving_cell: default_serving_cell()
                .into_iter()
                .filter(|f| r.serving_cell.contains(f))
                .collect(),
            neighbor_cells: r.neighbor_cells,
            lte_fallback: r.lte_fallback,
            cqi: r.cqi,
            mcs: r.mcs,
            bler: r.bler,
            harq: r.harq,
            drx: r.drx,
            bwp: r.bwp,
            mimo_layers: r.mimo_layers,
            tx_power: r.tx_power,
        },
        qos: QosLogging {
            qi_5: capability.qos.qi_5,
            qfi: capability.qos.qfi,
            slice_info: capability.qos.slice_info,
        },
        mobility: MobilityLogging {
            explicit_ho_events: capability.mobility.explicit_ho_events,
            rrc_state: capability.mobility.rrc_state,
            beam_switch: capability.mobility.beam_switch,
        },
        events: vec![
            GateableEvent::TestStart,
            GateableEvent::TestStop,
            GateableEvent::SignalLossStart,
            GateableEvent::SignalLossEnd,
        ],
        gps: capability.timestamp_sources.contains(&TimestampSource::Gps),
        clock_calibration_interval_sec: 30,
        capture: None,
        ping: None,
    }
}

fn default_serving_cell() -> Vec<ServingCellField> {
    use ServingCellField::*;
    vec![Pci, Nci, Tac, Nrarfcn, Rsrp, Rsrq, Sinr]
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_contracts::{MobilityCapability, QosCapability, RadioCapability};

    fn pixel_baseline() -> Capability {
        Capability {
            radio: RadioCapability {
                serving_cell: vec![
                    ServingCellField::Pci,
                    ServingCellField::Nci,
                    ServingCellField::Rsrp,
                    ServingCellField::Rsrq,
                    ServingCellField::Sinr,
                ],
                neighbor_cells: true,
                lte_fallback: true,
                cqi: false,
                mcs: false,
                bler: false,
                harq: false,
                drx: false,
                bwp: false,
                mimo_layers: false,
                tx_power: false,
            },
            qos: QosCapability {
                qi_5: false,
                qfi: false,
                slice_info: false,
            },
            mobility: MobilityCapability {
                explicit_ho_events: false,
                rrc_state: false,
                beam_switch: false,
            },
            timestamp_sources: vec![
                TimestampSource::Mono,
                TimestampSource::Utc,
                TimestampSource::Gps,
            ],
            supports_ping: true,
            monitor: None,
        }
    }

    #[test]
    fn effective_is_subset_of_capability() {
        let cap = pixel_baseline();
        let cfg = resolve_effective_config(&cap, 1);

        // serving_cell ⊆ capability.serving_cell (tac/nrarfcn dropped here)
        for f in &cfg.radio.serving_cell {
            assert!(
                cap.radio.serving_cell.contains(f),
                "{f:?} not in capability"
            );
        }
        assert!(!cfg.radio.serving_cell.contains(&ServingCellField::Tac));
        // a flag can't be enabled beyond capability
        assert!(!cfg.radio.cqi);
        assert!(cfg.radio.neighbor_cells); // capable → enabled
        assert!(cfg.gps); // gps in timestamp_sources
        assert_eq!(cfg.version, 1);
    }

    #[test]
    fn no_gps_capability_means_no_gps_logging() {
        let mut cap = pixel_baseline();
        cap.timestamp_sources = vec![TimestampSource::Mono, TimestampSource::Utc];
        let cfg = resolve_effective_config(&cap, 5);
        assert!(!cfg.gps);
        assert_eq!(cfg.version, 5);
    }
}
