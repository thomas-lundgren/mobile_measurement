package com.measurement.logger;

import android.Manifest;
import android.content.Intent;
import android.content.SharedPreferences;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.PowerManager;
import android.provider.Settings;
import android.widget.ArrayAdapter;
import android.widget.AutoCompleteTextView;
import android.widget.Button;
import android.widget.EditText;
import android.widget.TextView;

import androidx.annotation.Nullable;
import androidx.appcompat.app.AppCompatActivity;
import androidx.core.app.ActivityCompat;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.LinkedHashSet;
import java.util.List;

/** Operator UI: enter server URL (a dropdown of recently-used + typed entries,
 *  persisted in SharedPreferences) + enrollment token, start/stop the foreground
 *  LoggerService, and watch its state. */
public final class MainActivity extends AppCompatActivity {
    /** SharedPreferences key for the saved server-URL history (newline-joined,
     *  most-recently-used first). Stored alongside LoggerService's prefs. */
    private static final String PREFS = "mp_logger";
    private static final String KEY_URLS = "server_urls";

    private final Handler ui = new Handler(Looper.getMainLooper());

    private SharedPreferences prefs;
    private final List<String> savedUrls = new ArrayList<>();
    private ArrayAdapter<String> urlAdapter;

    @Override
    protected void onCreate(@Nullable Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_main);

        requestPermissions();
        requestBatteryExemption();
        prefs = getSharedPreferences(PREFS, MODE_PRIVATE);

        AutoCompleteTextView serverUrl = findViewById(R.id.serverUrl);
        EditText enrollToken = findViewById(R.id.enrollToken);
        TextView status = findViewById(R.id.status);

        // Load the URL history and wire it to the dropdown (typed values that
        // get used are remembered automatically — see rememberUrl below).
        savedUrls.addAll(loadUrls());
        urlAdapter = new ArrayAdapter<>(
                this, android.R.layout.simple_dropdown_item_1line, savedUrls);
        serverUrl.setAdapter(urlAdapter);
        serverUrl.setThreshold(0); // show the list on first character / focus
        serverUrl.setOnFocusChangeListener((v, hasFocus) -> {
            if (hasFocus && !savedUrls.isEmpty()) serverUrl.showDropDown();
        });
        serverUrl.setOnClickListener(v -> {
            if (!savedUrls.isEmpty()) serverUrl.showDropDown();
        });
        if (!savedUrls.isEmpty() && serverUrl.getText().length() == 0) {
            serverUrl.setText(savedUrls.get(0));
        }

        ((Button) findViewById(R.id.startBtn)).setOnClickListener(v -> {
            String url = serverUrl.getText().toString().trim();
            if (url.isEmpty()) return;
            rememberUrl(url);
            Intent i = new Intent(this, LoggerService.class)
                    .putExtra(LoggerService.EXTRA_SERVER, url)
                    .putExtra(LoggerService.EXTRA_TOKEN, enrollToken.getText().toString().trim());
            androidx.core.content.ContextCompat.startForegroundService(this, i);
        });

        ((Button) findViewById(R.id.stopBtn)).setOnClickListener(v ->
                stopService(new Intent(this, LoggerService.class)));

        // Dev/automation: `adb shell am start -n .../.MainActivity -e server URL -e token TOK`
        // prefills the fields and auto-starts the service. The URL is also
        // remembered so it shows up in the dropdown next time.
        String extraServer = getIntent().getStringExtra(LoggerService.EXTRA_SERVER);
        String extraToken = getIntent().getStringExtra(LoggerService.EXTRA_TOKEN);
        if (extraServer != null && !extraServer.isEmpty()) {
            serverUrl.setText(extraServer);
            if (extraToken != null) {
                enrollToken.setText(extraToken);
            }
            rememberUrl(extraServer);
            Intent i = new Intent(this, LoggerService.class)
                    .putExtra(LoggerService.EXTRA_SERVER, extraServer)
                    .putExtra(LoggerService.EXTRA_TOKEN, extraToken == null ? "" : extraToken);
            androidx.core.content.ContextCompat.startForegroundService(this, i);
        }

        // poll the service state into the text view
        ui.post(new Runnable() {
            @Override
            public void run() {
                status.setText(LoggerService.STATE);
                ui.postDelayed(this, 500);
            }
        });
    }

    /** Load saved URLs (newline-joined, most-recent first). First launch returns
     *  an empty list — the field is then blank and the user types one. */
    private List<String> loadUrls() {
        String raw = prefs.getString(KEY_URLS, "");
        if (raw.isEmpty()) return new ArrayList<>();
        List<String> out = new ArrayList<>();
        for (String s : raw.split("\n")) {
            if (!s.isEmpty()) out.add(s);
        }
        return out;
    }

    /** Move-to-top + persist (de-duplicated, cap at 10 most-recent). */
    private void rememberUrl(String url) {
        LinkedHashSet<String> set = new LinkedHashSet<>();
        set.add(url);
        set.addAll(savedUrls);
        savedUrls.clear();
        savedUrls.addAll(Arrays.asList(set.toArray(new String[0])));
        while (savedUrls.size() > 10) savedUrls.remove(savedUrls.size() - 1);
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < savedUrls.size(); i++) {
            if (i > 0) sb.append('\n');
            sb.append(savedUrls.get(i));
        }
        prefs.edit().putString(KEY_URLS, sb.toString()).apply();
        if (urlAdapter != null) urlAdapter.notifyDataSetChanged();
    }

    /** Doze and app-standby would throttle the logger's network and timers in
     *  a pocket; a measurement instrument must be exempt. Asks once per launch
     *  until granted (system dialog). */
    private void requestBatteryExemption() {
        PowerManager pm = (PowerManager) getSystemService(POWER_SERVICE);
        if (pm.isIgnoringBatteryOptimizations(getPackageName())) {
            return;
        }
        try {
            startActivity(new Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                    Uri.parse("package:" + getPackageName())));
        } catch (Exception ignored) {
            // no handler on this ROM — the user can still set it in Settings
        }
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
