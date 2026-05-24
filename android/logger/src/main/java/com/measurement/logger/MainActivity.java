package com.measurement.logger;

import android.Manifest;
import android.content.Intent;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.widget.Button;
import android.widget.EditText;
import android.widget.TextView;

import androidx.annotation.Nullable;
import androidx.appcompat.app.AppCompatActivity;
import androidx.core.app.ActivityCompat;

/** Minimal operator UI: enter server + enrollment token, start/stop the
 *  foreground LoggerService, and watch its state. */
public final class MainActivity extends AppCompatActivity {
    private final Handler ui = new Handler(Looper.getMainLooper());

    @Override
    protected void onCreate(@Nullable Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_main);

        requestPermissions();

        EditText serverUrl = findViewById(R.id.serverUrl);
        EditText enrollToken = findViewById(R.id.enrollToken);
        TextView status = findViewById(R.id.status);

        ((Button) findViewById(R.id.startBtn)).setOnClickListener(v -> {
            Intent i = new Intent(this, LoggerService.class)
                    .putExtra(LoggerService.EXTRA_SERVER, serverUrl.getText().toString().trim())
                    .putExtra(LoggerService.EXTRA_TOKEN, enrollToken.getText().toString().trim());
            androidx.core.content.ContextCompat.startForegroundService(this, i);
        });

        ((Button) findViewById(R.id.stopBtn)).setOnClickListener(v ->
                stopService(new Intent(this, LoggerService.class)));

        // poll the service state into the text view
        ui.post(new Runnable() {
            @Override
            public void run() {
                status.setText(LoggerService.STATE);
                ui.postDelayed(this, 500);
            }
        });
    }

    private void requestPermissions() {
        java.util.List<String> perms = new java.util.ArrayList<>();
        perms.add(Manifest.permission.ACCESS_FINE_LOCATION);
        perms.add(Manifest.permission.READ_PHONE_STATE);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            perms.add(Manifest.permission.POST_NOTIFICATIONS);
        }
        ActivityCompat.requestPermissions(this, perms.toArray(new String[0]), 1);
    }
}
