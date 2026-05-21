package spike;

public class DualClockTest {
    public static void main(String[] args) {
        final int iterations = 100;
        long maxMonoDeltaNs = 0;
        long maxUtcDeltaMs = 0;

        for (int i = 0; i < iterations; i++) {
            long javaMonoBefore = System.nanoTime();
            long javaUtcBefore = System.currentTimeMillis();

            String rustJson = RustBridge.sampleClock();

            long javaMonoAfter = System.nanoTime();
            long javaUtcAfter = System.currentTimeMillis();

            // Parse Rust JSON: {"mono_ns":123, "utc_ns":456}
            rustJson = rustJson.replaceAll("[\\{}\\\"]", "");
            String[] parts = rustJson.split(",");
            long rustMono = Long.parseLong(parts[0].split(":")[1].trim());
            long rustUtc = Long.parseLong(parts[1].split(":")[1].trim());

            long javaMono = (javaMonoBefore + javaMonoAfter) / 2;
            long javaUtc = (javaUtcBefore + javaUtcAfter) / 2;

            long monoDeltaNs = Math.abs(rustMono - javaMono);
            long utcDeltaMs = Math.abs((rustUtc / 1_000_000) - javaUtc);

            if (monoDeltaNs > maxMonoDeltaNs) maxMonoDeltaNs = monoDeltaNs;
            if (utcDeltaMs > maxUtcDeltaMs) maxUtcDeltaMs = utcDeltaMs;
        }

        System.out.println("Dual-clock agreement over " + iterations + " samples:");
        System.out.println("  max mono delta (ns): " + maxMonoDeltaNs);
        System.out.println("  max UTC delta (ms):  " + maxUtcDeltaMs);

        long monoThresholdNs = 5_000_000; // 5 ms
        long utcThresholdMs = 10;         // 10 ms

        boolean pass = maxMonoDeltaNs < monoThresholdNs && maxUtcDeltaMs < utcThresholdMs;
        System.out.println("PASS: " + pass);
        if (!pass) {
            System.exit(1);
        }
    }
}
