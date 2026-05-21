package spike;

import java.io.File;

public class ResilienceHarness {
    public static void main(String[] args) throws Exception {
        String logDir = "spikes/stage-a-harness/logs_resilience";
        new File(logDir).mkdirs();

        String config = String.format(
            "{\"node_id\":\"desktop-jvm-1\",\"log_dir\":\"%s\",\"segment_rows\":50}",
            logDir
        );

        long handle = RustBridge.init(config);

        String flow = "{"
            + "\"flow_id\":1,"
            + "\"server_host\":\"127.0.0.1\","
            + "\"server_port\":41000,"
            + "\"rate_pps\":20,"
            + "\"packet_size\":64,"
            + "\"duration_sec\":20"
            + "}";

        RustBridge.startFlow(handle, flow);
        System.out.println("FLOW_STARTED");

        // Wait for the flow to finish on its own
        Thread.sleep(25000);

        String status = RustBridge.pollStatus(handle);
        System.out.println("FINAL_STATUS: " + status);

        RustBridge.stop(handle);
        RustBridge.destroy(handle);

        File dir = new File(logDir);
        File[] files = dir.listFiles((d, name) -> name.endsWith(".parquet"));
        if (files != null) {
            System.out.println("SEGMENTS: " + files.length);
        }
    }
}
