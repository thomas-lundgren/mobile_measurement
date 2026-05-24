package com.measurement.logger;

/**
 * JNI bridge to the shared Rust core (libfieldtest_core). The Android Logger's
 * Java shell will use exactly this class; the desktop harness exercises the
 * same boundary on x86_64 before it runs under ART.
 *
 * Static natives take an explicit handle (a *RustCore) from {@link #coreNew()}.
 */
public final class RustCore {
    static {
        System.loadLibrary("fieldtest_core");
    }

    private RustCore() {}

    /** Allocate a native core handle. */
    public static native long coreNew();

    /** Start a test from a UeTestConfig JSON blob. Returns 0 on success. */
    public static native int startTest(long handle, String configJson);

    /** Live per-flow counters as a JSON array. */
    public static native String pollStatus(long handle);

    /** Stop the test, seal logs, return per-flow summaries as JSON. */
    public static native String stopTest(long handle);

    /** JSON array of sealed *.parquet paths to upload. */
    public static native String sealedFiles(long handle);

    /** Dual-clock sample as JSON ({mono_ns, utc_ns}). */
    public static native String sampleClock();

    /** Release the native handle. */
    public static native void destroy(long handle);
}
