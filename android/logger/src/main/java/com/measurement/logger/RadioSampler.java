package com.measurement.logger;

import android.Manifest;
import android.content.Context;
import android.content.pm.PackageManager;
import android.os.Build;
import android.telephony.CellInfo;
import android.telephony.CellInfoLte;
import android.telephony.CellInfoNr;
import android.telephony.CellSignalStrengthLte;
import android.telephony.CellSignalStrengthNr;
import android.telephony.CellIdentityNr;
import android.telephony.CellIdentityLte;
import android.telephony.TelephonyManager;

import androidx.core.content.ContextCompat;

import org.json.JSONArray;
import org.json.JSONObject;

import java.util.List;

/** Polls Telephony (~1 Hz) and feeds serving-cell + neighbor metrics into the
 *  core (pushRadio). Pixel 9 Pro reference: NR serving cell + neighbor PCIs.
 *  Capability-dependent fields absent on the device are simply omitted. */
public final class RadioSampler {
    private final Context ctx;
    private final long handle;
    private volatile boolean running;
    private Thread thread;

    public RadioSampler(Context ctx, long handle) {
        this.ctx = ctx;
        this.handle = handle;
    }

    public void start() {
        if (ContextCompat.checkSelfPermission(ctx, Manifest.permission.ACCESS_FINE_LOCATION)
                != PackageManager.PERMISSION_GRANTED) {
            return;
        }
        running = true;
        thread = new Thread(this::loop, "radio-sampler");
        thread.start();
    }

    public void stop() {
        running = false;
        if (thread != null) {
            try {
                thread.join(1500);
            } catch (InterruptedException ignored) {
            }
        }
    }

    private void loop() {
        TelephonyManager tm = (TelephonyManager) ctx.getSystemService(Context.TELEPHONY_SERVICE);
        while (running) {
            try {
                sampleOnce(tm);
            } catch (RuntimeException ignored) {
                // includes SecurityException (a RuntimeException)
            }
            try {
                Thread.sleep(1000L);
            } catch (InterruptedException e) {
                return;
            }
        }
    }

    @SuppressWarnings("MissingPermission")
    private void sampleOnce(TelephonyManager tm) {
        List<CellInfo> cells = tm.getAllCellInfo();
        if (cells == null) {
            return;
        }
        long[] t = Clock.now();
        JSONObject row = new JSONObject();
        JSONArray neighbors = new JSONArray();
        try {
            row.put("mono_ns", t[0]);
            row.put("utc_ns", t[1]);
            boolean haveServing = false;

            for (CellInfo ci : cells) {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q && ci instanceof CellInfoNr) {
                    CellInfoNr nr = (CellInfoNr) ci;
                    CellIdentityNr id = (CellIdentityNr) nr.getCellIdentity();
                    CellSignalStrengthNr ss = (CellSignalStrengthNr) nr.getCellSignalStrength();
                    if (ci.isRegistered() && !haveServing) {
                        haveServing = true;
                        row.put("pci", id.getPci());
                        row.put("nci", id.getNci());
                        row.put("tac", id.getTac());
                        row.put("nrarfcn", id.getNrarfcn());
                        row.put("rsrp", ss.getSsRsrp());
                        row.put("rsrq", ss.getSsRsrq());
                        row.put("sinr", ss.getSsSinr());
                        row.put("cell_connection_status", "PRIMARY");
                    } else {
                        neighbors.put(neighbor(id.getPci(), ss.getSsRsrp()));
                    }
                } else if (ci instanceof CellInfoLte) {
                    CellInfoLte lte = (CellInfoLte) ci;
                    CellIdentityLte id = (CellIdentityLte) lte.getCellIdentity();
                    CellSignalStrengthLte ss = (CellSignalStrengthLte) lte.getCellSignalStrength();
                    if (ci.isRegistered() && !haveServing) {
                        haveServing = true;
                        row.put("lte_pci", id.getPci());
                        row.put("earfcn", id.getEarfcn());
                        row.put("lte_rsrp", ss.getRsrp());
                        row.put("cell_connection_status", "PRIMARY");
                    } else {
                        neighbors.put(neighbor(id.getPci(), ss.getRsrp()));
                    }
                }
            }
            if (neighbors.length() > 0) {
                row.put("neighbor_cells", neighbors);
            }
            RustCore.pushRadio(handle, row.toString());
        } catch (Exception ignored) {
        }
    }

    private static JSONObject neighbor(int pci, int rsrp) throws Exception {
        JSONObject n = new JSONObject();
        n.put("pci", pci);
        n.put("rsrp", rsrp);
        return n;
    }
}
