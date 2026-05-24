package com.measurement.logger;

import android.Manifest;
import android.content.Context;
import android.content.pm.PackageManager;
import android.location.Location;
import android.location.LocationListener;
import android.location.LocationManager;
import android.os.Looper;

import androidx.core.content.ContextCompat;

import org.json.JSONObject;

/** Feeds GPS fixes into the core (pushGps). Uses the framework LocationManager
 *  (no Play Services dependency). */
public final class GpsSampler {
    private final Context ctx;
    private final long handle;
    private LocationManager lm;
    private LocationListener listener;

    public GpsSampler(Context ctx, long handle) {
        this.ctx = ctx;
        this.handle = handle;
    }

    public void start() {
        if (ContextCompat.checkSelfPermission(ctx, Manifest.permission.ACCESS_FINE_LOCATION)
                != PackageManager.PERMISSION_GRANTED) {
            return; // no permission → no GPS rows (logs remain valid, just no gps.parquet)
        }
        lm = (LocationManager) ctx.getSystemService(Context.LOCATION_SERVICE);
        listener = this::onLocation;
        try {
            lm.requestLocationUpdates(LocationManager.GPS_PROVIDER, 1000L, 0f, listener, Looper.getMainLooper());
        } catch (SecurityException | IllegalArgumentException ignored) {
        }
    }

    public void stop() {
        if (lm != null && listener != null) {
            lm.removeUpdates(listener);
        }
    }

    private void onLocation(Location loc) {
        long[] t = Clock.now();
        try {
            JSONObject o = new JSONObject();
            o.put("mono_ns", t[0]);
            o.put("utc_ns", t[1]);
            o.put("latitude", loc.getLatitude());
            o.put("longitude", loc.getLongitude());
            if (loc.hasAltitude()) {
                o.put("altitude_m", loc.getAltitude());
            }
            if (loc.hasSpeed()) {
                o.put("speed_mps", loc.getSpeed());
            }
            if (loc.hasBearing()) {
                o.put("heading_deg", loc.getBearing());
            }
            if (loc.hasAccuracy()) {
                o.put("accuracy_m", loc.getAccuracy());
            }
            RustCore.pushGps(handle, o.toString());
        } catch (Exception ignored) {
        }
    }
}
