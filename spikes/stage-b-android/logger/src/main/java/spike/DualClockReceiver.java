package spike;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.util.Log;

import org.json.JSONObject;

public class DualClockReceiver extends BroadcastReceiver {
    private static final String TAG = "DualClockTest";

    @Override
    public void onReceive(Context context, Intent intent) {
        final int iterations = 50;
        long maxMonoDeltaNs = 0;
        long maxUtcDeltaMs = 0;

        for (int i = 0; i < iterations; i++) {
            long javaMonoBefore = System.nanoTime();
            long javaUtcBefore = System.currentTimeMillis();

            String rustJson = RustBridge.sampleClock();

            long javaMonoAfter = System.nanoTime();
            long javaUtcAfter = System.currentTimeMillis();

            try {
                JSONObject obj = new JSONObject(rustJson);
                long rustMono = obj.getLong("mono_ns");
                long rustUtc = obj.getLong("utc_ns");

                long javaMono = (javaMonoBefore + javaMonoAfter) / 2;
                long javaUtc = (javaUtcBefore + javaUtcAfter) / 2;

                long monoDeltaNs = Math.abs(rustMono - javaMono);
                long utcDeltaMs = Math.abs((rustUtc / 1_000_000) - javaUtc);

                if (monoDeltaNs > maxMonoDeltaNs) maxMonoDeltaNs = monoDeltaNs;
                if (utcDeltaMs > maxUtcDeltaMs) maxUtcDeltaMs = utcDeltaMs;
            } catch (Exception e) {
                Log.e(TAG, "Parse error: " + e.getMessage());
                return;
            }
        }

        Log.i(TAG, "Dual-clock agreement over " + iterations + " samples:");
        Log.i(TAG, "  max mono delta (ns): " + maxMonoDeltaNs);
        Log.i(TAG, "  max UTC delta (ms):  " + maxUtcDeltaMs);

        boolean pass = maxMonoDeltaNs < 5_000_000 && maxUtcDeltaMs < 10;
        Log.i(TAG, "PASS: " + pass);
    }
}
