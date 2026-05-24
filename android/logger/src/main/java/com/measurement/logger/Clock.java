package com.measurement.logger;

import org.json.JSONObject;

/** Dual-clock sample taken from the native core, so device-side rows share the
 *  core's clock. Returns [mono_ns, utc_ns]. */
final class Clock {
    private Clock() {}

    static long[] now() {
        try {
            JSONObject o = new JSONObject(RustCore.sampleClock());
            return new long[] {o.optLong("mono_ns"), o.optLong("utc_ns")};
        } catch (Exception e) {
            return new long[] {0L, 0L};
        }
    }
}
