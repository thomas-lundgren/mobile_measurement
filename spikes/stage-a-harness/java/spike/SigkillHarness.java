package spike;

import java.io.File;

public class SigkillHarness {
    public static void main(String[] args) throws Exception {
        String logDir = "spikes/stage-a-harness/logs_sigkill";
        new File(logDir).mkdirs();

        String config = String.format(
            "{\"node_id\":\"desktop-jvm-1\",\"log_dir\":\"%s\",\"segment_rows\":30}",
            logDir
        );

        long handle = RustBridge.init(config);

        String flow = "{"
            + "\"flow_id\":1,"
            + "\"server_host\":\"127.0.0.1\","
            + "\"server_port\":41000,"
            + "\"rate_pps\":50,"
            + "\"packet_size\":64,"
            + "\"duration_sec\":60"
            + "}";

        RustBridge.startFlow(handle, flow);
        System.out.println("FLOW_STARTED");

        // Keep process alive until killed
        Thread.sleep(120_000);
    }
}
