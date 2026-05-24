package com.measurement.logger;

/**
 * JNI bridge to the shared Rust core (libfieldtest_core). Same class the
 * desktop harness uses (core/jni_harness) — validated on x86_64 before ART.
 * Static natives take an explicit handle (a *RustCore) from {@link #coreNew()}.
 */
public final class RustCore {
    static {
        System.loadLibrary("fieldtest_core");
    }

    private RustCore() {}

    public static native long coreNew();

    /** Start a test from a UeTestConfig JSON blob. Returns 0 on success. */
    public static native int startTest(long handle, String configJson);

    /** Buffer a GPS row (JSON) for this test's gps.parquet. 0 on success. */
    public static native int pushGps(long handle, String rowJson);

    /** Buffer a clock-calibration row (JSON) for clock.parquet. 0 on success. */
    public static native int pushClock(long handle, String rowJson);

    /** Buffer a radio metric row (JSON) for radio.parquet. 0 on success. */
    public static native int pushRadio(long handle, String rowJson);

    /** Live per-flow counters as a JSON array. */
    public static native String pollStatus(long handle);

    /** Stop the test, seal logs, return per-flow summaries as JSON. */
    public static native String stopTest(long handle);

    /** JSON array of sealed *.parquet paths to upload. */
    public static native String sealedFiles(long handle);

    /** Dual-clock sample as JSON ({mono_ns, utc_ns}). */
    public static native String sampleClock();

    public static native void destroy(long handle);
}
