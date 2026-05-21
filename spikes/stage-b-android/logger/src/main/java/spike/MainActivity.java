package spike;

import android.Manifest;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.os.Build;
import android.os.Bundle;
import android.widget.Button;
import android.widget.EditText;
import android.widget.TextView;

import androidx.annotation.NonNull;
import androidx.appcompat.app.AppCompatActivity;
import androidx.core.app.ActivityCompat;
import androidx.core.content.ContextCompat;

public class MainActivity extends AppCompatActivity {
    private static final int PERM_REQ = 1;

    private EditText etHost;
    private EditText etPort;
    private EditText etDuration;
    private Button btnStart;
    private Button btnStop;
    private TextView tvStatus;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_main);

        etHost = findViewById(R.id.etHost);
        etPort = findViewById(R.id.etPort);
        etDuration = findViewById(R.id.etDuration);
        btnStart = findViewById(R.id.btnStart);
        btnStop = findViewById(R.id.btnStop);
        tvStatus = findViewById(R.id.tvStatus);

        etHost.setText("192.168.1.100");
        etPort.setText("41000");
        etDuration.setText("300");

        btnStart.setOnClickListener(v -> startTest());
        btnStop.setOnClickListener(v -> stopTest());

        requestPermissions();
    }

    private void requestPermissions() {
        String[] perms;
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            perms = new String[]{Manifest.permission.POST_NOTIFICATIONS, Manifest.permission.INTERNET};
        } else {
            perms = new String[]{Manifest.permission.INTERNET};
        }
        for (String p : perms) {
            if (ContextCompat.checkSelfPermission(this, p) != PackageManager.PERMISSION_GRANTED) {
                ActivityCompat.requestPermissions(this, perms, PERM_REQ);
                break;
            }
        }
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, @NonNull String[] permissions, @NonNull int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
    }

    private void startTest() {
        String host = etHost.getText().toString().trim();
        int port = Integer.parseInt(etPort.getText().toString().trim());
        int duration = Integer.parseInt(etDuration.getText().toString().trim());

        tvStatus.setText("Starting...");
        Intent intent = new Intent(this, LoggerService.class);
        intent.putExtra("server_host", host);
        intent.putExtra("server_port", port);
        intent.putExtra("duration_sec", duration);
        intent.putExtra("rate_pps", 10);
        startForegroundService(intent);
        tvStatus.setText("Service started");
    }

    private void stopTest() {
        Intent intent = new Intent(this, LoggerService.class);
        stopService(intent);
        tvStatus.setText("Service stopped");
    }
}
