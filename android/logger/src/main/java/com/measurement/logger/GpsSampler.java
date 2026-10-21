package com.measurement.logger;

import android.Manifest;
import android.content.Context;
import android.content.pm.PackageManager;
import android.location.Location;
import android.location.LocationListener;
import android.location.LocationManager;
import android.location.LocationRequest;
import android.os.Build;
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
            // GPS_PROVIDER is GNSS only: no fix indoors, in trains or tunnels,
            // so whole tests got no position. The fused provider uses GNSS when
            // it has it and Wi-Fi/cell positioning otherwise; accuracy_m tells
            // the two apart.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S
                    && lm.hasProvider(LocationManager.FUSED_PROVIDER)) {
                LocationRequest req = new LocationRequest.Builder(1000L)
                        .setQuality(LocationRequest.QUALITY_HIGH_ACCURACY)
                        .setMinUpdateIntervalMillis(1000L)
                        .build();
                lm.requestLocationUpdates(LocationManager.FUSED_PROVIDER, req,
                        ctx.getMainExecutor(), listener);
            } else {
                lm.requestLocationUpdates(LocationManager.GPS_PROVIDER, 1000L, 0f, listener,
                        Looper.getMainLooper());
            }
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
            o.put(Contract.GpsRow.MONO_NS, t[0]);
            o.put(Contract.GpsRow.UTC_NS, t[1]);
            o.put(Contract.GpsRow.LATITUDE, loc.getLatitude());
            o.put(Contract.GpsRow.LONGITUDE, loc.getLongitude());
            if (loc.hasAltitude()) {
                o.put(Contract.GpsRow.ALTITUDE_M, loc.getAltitude());
            }
            if (loc.hasSpeed()) {
                o.put(Contract.GpsRow.SPEED_MPS, loc.getSpeed());
            }
            if (loc.hasBearing()) {
                o.put(Contract.GpsRow.HEADING_DEG, loc.getBearing());
            }
            if (loc.hasAccuracy()) {
                o.put(Contract.GpsRow.ACCURACY_M, loc.getAccuracy());
            }
            RustCore.pushGps(handle, o.toString());
        } catch (Exception ignored) {
        }
    }
}
