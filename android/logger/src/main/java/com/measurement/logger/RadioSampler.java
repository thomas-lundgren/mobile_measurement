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
import android.telephony.SignalStrength;
import android.telephony.TelephonyManager;

import androidx.core.content.ContextCompat;

import org.json.JSONArray;
import org.json.JSONObject;

import java.util.List;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

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
                sampleOnce(tm, freshCellInfo(tm));
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

    /** getAllCellInfo() returns the platform's cached snapshot, which can sit
     *  unchanged for minutes — useless for tracking handovers. Ask the modem
     *  for a fresh one (API 29+), falling back to the cache if it doesn't
     *  answer within ~800 ms. */
    @SuppressWarnings("MissingPermission")
    private List<CellInfo> freshCellInfo(TelephonyManager tm) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            final List<CellInfo>[] out = new List[1];
            final CountDownLatch done = new CountDownLatch(1);
            try {
                tm.requestCellInfoUpdate(Runnable::run, new TelephonyManager.CellInfoCallback() {
                    @Override
                    public void onCellInfo(List<CellInfo> cells) {
                        out[0] = cells;
                        done.countDown();
                    }

                    @Override
                    public void onError(int errorCode, Throwable detail) {
                        done.countDown();
                    }
                });
                if (done.await(800, TimeUnit.MILLISECONDS) && out[0] != null) {
                    return out[0];
                }
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            } catch (RuntimeException ignored) {
            }
        }
        return tm.getAllCellInfo();
    }

    @SuppressWarnings("MissingPermission")
    private void sampleOnce(TelephonyManager tm, List<CellInfo> cells) {
        if (cells == null) {
            return;
        }
        long[] t = Clock.now();
        JSONObject row = new JSONObject();
        JSONArray neighbors = new JSONArray();
        try {
            row.put("mono_ns", t[0]);
            row.put("utc_ns", t[1]);
            boolean haveNr = false;
            boolean haveLte = false;

            for (CellInfo ci : cells) {
                int status = ci.getCellConnectionStatus();
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q && ci instanceof CellInfoNr) {
                    CellInfoNr nr = (CellInfoNr) ci;
                    CellIdentityNr id = (CellIdentityNr) nr.getCellIdentity();
                    CellSignalStrengthNr ss = (CellSignalStrengthNr) nr.getCellSignalStrength();
                    // SA: the NR cell is the registered primary. NSA: it is the
                    // secondary serving cell beside the LTE anchor — that is
                    // still the 5G leg the device is using, not a neighbor.
                    boolean primary = ci.isRegistered();
                    boolean secondary = status == CellInfo.CONNECTION_SECONDARY_SERVING;
                    if ((primary || secondary) && !haveNr) {
                        haveNr = true;
                        putIfValid(row, "pci", id.getPci());
                        putIfValid(row, "nci", id.getNci());
                        putIfValid(row, "tac", id.getTac());
                        putIfValid(row, "nrarfcn", id.getNrarfcn());
                        putIfValid(row, "rsrp", ss.getSsRsrp());
                        putIfValid(row, "rsrq", ss.getSsRsrq());
                        putIfValid(row, "sinr", ss.getSsSinr());
                        row.put("cell_connection_status", primary ? "PRIMARY" : "SECONDARY");
                    } else {
                        neighbors.put(neighbor(id.getPci(), ss.getSsRsrp()));
                    }
                } else if (ci instanceof CellInfoLte) {
                    CellInfoLte lte = (CellInfoLte) ci;
                    CellIdentityLte id = (CellIdentityLte) lte.getCellIdentity();
                    CellSignalStrengthLte ss = (CellSignalStrengthLte) lte.getCellSignalStrength();
                    if (ci.isRegistered() && !haveLte) {
                        haveLte = true;
                        putIfValid(row, "lte_pci", id.getPci());
                        putIfValid(row, "earfcn", id.getEarfcn());
                        putIfValid(row, "lte_rsrp", ss.getRsrp());
                    } else {
                        neighbors.put(neighbor(id.getPci(), ss.getRsrp()));
                    }
                }
            }
            // NSA: CellInfoNr often carries no signal (or no NR entry at all);
            // the NR leg's measurements are reported via SignalStrength.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q && !row.has("rsrp")) {
                SignalStrength sig = tm.getSignalStrength();
                if (sig != null) {
                    for (CellSignalStrengthNr ss : sig.getCellSignalStrengths(CellSignalStrengthNr.class)) {
                        if (valid(ss.getSsRsrp())) {
                            putIfValid(row, "rsrp", ss.getSsRsrp());
                            putIfValid(row, "rsrq", ss.getSsRsrq());
                            putIfValid(row, "sinr", ss.getSsSinr());
                            if (!row.has("cell_connection_status")) {
                                row.put("cell_connection_status", haveLte ? "SECONDARY" : "PRIMARY");
                            }
                            break;
                        }
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

    /** Telephony reports "not available" as CellInfo.UNAVAILABLE
     *  (Integer.MAX_VALUE) / UNAVAILABLE_LONG; those must be null, not dBm. */
    private static boolean valid(long v) {
        return v != CellInfo.UNAVAILABLE && v != CellInfo.UNAVAILABLE_LONG;
    }

    private static void putIfValid(JSONObject row, String key, long v) throws Exception {
        if (valid(v)) {
            row.put(key, v);
        }
    }

    private static JSONObject neighbor(int pci, int rsrp) throws Exception {
        JSONObject n = new JSONObject();
        n.put("pci", pci);
        putIfValid(n, "rsrp", rsrp);
        return n;
    }
}
