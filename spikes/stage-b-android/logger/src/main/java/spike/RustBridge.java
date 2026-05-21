package spike;

public class RustBridge {
    static {
        System.loadLibrary("fieldtest_core");
    }

    public static native long init(String configJson);
    public static native void startFlow(long handle, String flowJson);
    public static native String pollStatus(long handle);
    public static native void stop(long handle);
    public static native void destroy(long handle);
    public static native String sampleClock();
}
