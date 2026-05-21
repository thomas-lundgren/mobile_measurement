package spike;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Intent;
import android.os.Build;
import android.os.IBinder;
import android.util.Log;

import androidx.core.app.NotificationCompat;

import org.json.JSONException;
import org.json.JSONObject;

import java.io.File;

public class LoggerService extends Service {
    private static final String TAG = "LoggerService";
    private static final String CHANNEL_ID = "fieldtest_channel";
    private static final int NOTIF_ID = 1;

    private long handle = 0;
    private volatile boolean running = false;
    private Thread worker;

    @Override
    public void onCreate() {
        super.onCreate();
        createNotificationChannel();
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        String serverHost = intent != null ? intent.getStringExtra("server_host") : null;
        int serverPort = intent != null ? intent.getIntExtra("server_port", 41000) : 41000;
        int durationSec = intent != null ? intent.getIntExtra("duration_sec", 300) : 300;
        int ratePps = intent != null ? intent.getIntExtra("rate_pps", 10) : 10;

        if (serverHost == null) {
            Log.e(TAG, "No server_host provided");
            stopSelf();
            return START_NOT_STICKY;
        }

        startForeground(NOTIF_ID, buildNotification("Starting test..."));

        File logDir = new File(getExternalFilesDir(null), "fieldtest_logs");
        logDir.mkdirs();

        String config = String.format(
            "{\"node_id\":\"pixel9pro_stageb\",\"log_dir\":\"%s\",\"segment_rows\":100}",
            logDir.getAbsolutePath()
        );

        handle = RustBridge.init(config);
        Log.i(TAG, "Rust init handle=" + handle);

        String flow = String.format(
            "{\"flow_id\":1,\"server_host\":\"%s\",\"server_port\":%d,\"rate_pps\":%d,\"packet_size\":64,\"duration_sec\":%d}",
            serverHost, serverPort, ratePps, durationSec
        );

        RustBridge.startFlow(handle, flow);
        running = true;

        worker = new Thread(() -> {
            while (running) {
                String status = RustBridge.pollStatus(handle);
                Log.i(TAG, "status: " + status);
                updateNotification("Running: " + status);
                try {
                    Thread.sleep(1000);
                } catch (InterruptedException e) {
                    break;
                }
            }
            Log.i(TAG, "Worker exiting");
        });
        worker.start();

        return START_NOT_STICKY;
    }

    @Override
    public void onDestroy() {
        running = false;
        if (worker != null) {
            worker.interrupt();
            try {
                worker.join(2000);
            } catch (InterruptedException e) {
                // ignore
            }
        }
        if (handle != 0) {
            RustBridge.stop(handle);
            RustBridge.destroy(handle);
            handle = 0;
        }
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    private void createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            NotificationChannel channel = new NotificationChannel(
                    CHANNEL_ID,
                    "FieldTest Measurements",
                    NotificationManager.IMPORTANCE_LOW
            );
            channel.setDescription("Foreground service for network measurements");
            NotificationManager manager = getSystemService(NotificationManager.class);
            manager.createNotificationChannel(channel);
        }
    }

    private Notification buildNotification(String text) {
        Intent intent = new Intent(this, MainActivity.class);
        PendingIntent pendingIntent = PendingIntent.getActivity(
                this, 0, intent, PendingIntent.FLAG_IMMUTABLE);

        return new NotificationCompat.Builder(this, CHANNEL_ID)
                .setContentTitle("FieldTest Stage B")
                .setContentText(text)
                .setSmallIcon(android.R.drawable.ic_menu_compass)
                .setContentIntent(pendingIntent)
                .setOngoing(true)
                .build();
    }

    private void updateNotification(String text) {
        NotificationManager manager = getSystemService(NotificationManager.class);
        manager.notify(NOTIF_ID, buildNotification(text));
    }
}
