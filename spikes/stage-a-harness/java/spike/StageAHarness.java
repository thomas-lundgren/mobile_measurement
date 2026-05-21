package spike;

import java.io.File;

public class StageAHarness {
    public static void main(String[] args) throws Exception {
        String logDir = "spikes/stage-a-harness/logs";
        new File(logDir).mkdirs();

        String config = String.format(
            "{\"node_id\":\"desktop-jvm-1\",\"log_dir\":\"%s\",\"segment_rows\":100}",
            logDir
        );

        long handle = RustBridge.init(config);
        System.out.println("JNI init OK, handle=" + handle);

        String flow = "{"
            + "\"flow_id\":1,"
            + "\"server_host\":\"127.0.0.1\","
            + "\"server_port\":41000,"
            + "\"rate_pps\":10,"
            + "\"packet_size\":64,"
            + "\"duration_sec\":5"
            + "}";

        RustBridge.startFlow(handle, flow);
        System.out.println("Flow started");

        for (int i = 0; i < 10; i++) {
            Thread.sleep(500);
            String status = RustBridge.pollStatus(handle);
            System.out.println("status: " + status);
        }

        RustBridge.stop(handle);
        System.out.println("Flow stopped");

        RustBridge.destroy(handle);
        System.out.println("Handle destroyed");

        // List generated parquet files
        File dir = new File(logDir);
        File[] files = dir.listFiles((d, name) -> name.endsWith(".parquet"));
        if (files != null) {
            System.out.println("Parquet segments created: " + files.length);
            for (File f : files) {
                System.out.println("  " + f.getName() + " size=" + f.length());
            }
        }
    }
}
