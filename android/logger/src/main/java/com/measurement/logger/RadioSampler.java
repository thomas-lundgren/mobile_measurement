package com.measurement.logger;

import android.Manifest;
import android.content.Context;
import android.content.pm.PackageManager;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.wifi.ScanResult;
import android.net.wifi.WifiInfo;
import android.net.wifi.WifiManager;
import android.os.Build;
import android.telephony.CellInfo;
import android.telephony.CellInfoLte;
import android.telephony.CellInfoNr;
import android.telephony.CellSignalStrengthLte;
import android.telephony.CellSignalStrengthNr;
import android.telephony.CellIdentityNr;
import android.telephony.CellIdentityLte;
import android.telephony.ServiceState;
import android.telephony.SignalStrength;
import android.telephony.TelephonyCallback;
import android.telephony.TelephonyDisplayInfo;
import android.telephony.TelephonyManager;

import androidx.annotation.RequiresApi;
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
    /** Radio mode from TelephonyDisplayInfo (null until reported, or before API 31). */
    private volatile String networkType;
    private TelephonyCallback displayCallback;

    public RadioSampler(Context ctx, long handle) {
        this.ctx = ctx;
        this.handle = handle;
    }

    public void start() {
        if (ContextCompat.checkSelfPermission(ctx, Manifest.permission.ACCESS_FINE_LOCATION)
                != PackageManager.PERMISSION_GRANTED) {
            return;
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            listenDisplayInfo();
        }
        running = true;
        thread = new Thread(this::loop, "radio-sampler");
        thread.start();
    }

    public void stop() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S && displayCallback != null) {
            TelephonyManager tm = (TelephonyManager) ctx.getSystemService(Context.TELEPHONY_SERVICE);
            tm.unregisterTelephonyCallback(displayCallback);
            displayCallback = null;
        }
        running = false;
        if (thread != null) {
            try {
                thread.join(1500);
            } catch (InterruptedException ignored) {
            }
        }
    }

    /** In NSA the modem adds and drops the 5G leg all the time (LTE_CA for
     *  minutes on a walk) while CellInfo keeps listing NR cells as secondary
     *  serving. TelephonyDisplayInfo says which mode the device is in; it
     *  is reported on registration and on every change. */
    @RequiresApi(Build.VERSION_CODES.S)
    private void listenDisplayInfo() {
        TelephonyManager tm = (TelephonyManager) ctx.getSystemService(Context.TELEPHONY_SERVICE);
        DisplayInfoCallback cb = new DisplayInfoCallback();
        try {
            tm.registerTelephonyCallback(Runnable::run, cb);
            displayCallback = cb;
        } catch (RuntimeException ignored) {
            // no READ_PHONE_STATE: network_type stays null, NR handled as before
        }
    }

    @RequiresApi(Build.VERSION_CODES.S)
    private final class DisplayInfoCallback extends TelephonyCallback
            implements TelephonyCallback.DisplayInfoListener {
        @Override
        public void onDisplayInfoChanged(TelephonyDisplayInfo info) {
            networkType = describe(info);
        }
    }

    @RequiresApi(Build.VERSION_CODES.R)
    static String describe(TelephonyDisplayInfo info) {
        switch (info.getOverrideNetworkType()) {
            case TelephonyDisplayInfo.OVERRIDE_NETWORK_TYPE_NR_NSA:
                return "NR_NSA";
            case TelephonyDisplayInfo.OVERRIDE_NETWORK_TYPE_NR_ADVANCED:
                return "NR_ADVANCED";
            case TelephonyDisplayInfo.OVERRIDE_NETWORK_TYPE_LTE_CA:
                return "LTE_CA";
            case TelephonyDisplayInfo.OVERRIDE_NETWORK_TYPE_LTE_ADVANCED_PRO:
                return "LTE_ADVANCED_PRO";
            default:
                break;
        }
        switch (info.getNetworkType()) {
            case TelephonyManager.NETWORK_TYPE_NR:
                return "NR_SA";
            case TelephonyManager.NETWORK_TYPE_LTE:
                return "LTE";
            case TelephonyManager.NETWORK_TYPE_UNKNOWN:
                return null;
            default:
                return "OTHER_" + info.getNetworkType();
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
    private static List<CellInfo> freshCellInfo(TelephonyManager tm) {
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

    /** The serving cell for the status report (RadioSnapshot: pci, nci and
     *  signal): the NR cell (SA primary or NSA secondary), else the LTE cell.
     *  Lets the server see the phone move between tests. Null if unknown. */
    @SuppressWarnings("MissingPermission")
    static JSONObject serving(Context ctx) {
        try {
            TelephonyManager tm = (TelephonyManager) ctx.getSystemService(Context.TELEPHONY_SERVICE);
            List<CellInfo> cells = freshCellInfo(tm);
            if (cells == null) {
                return null;
            }
            JSONObject lte = null;
            for (CellInfo ci : cells) {
                boolean serving = ci.isRegistered()
                        || ci.getCellConnectionStatus() == CellInfo.CONNECTION_SECONDARY_SERVING;
                if (!serving) {
                    continue;
                }
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q && ci instanceof CellInfoNr) {
                    CellIdentityNr id = (CellIdentityNr) ci.getCellIdentity();
                    JSONObject row = new JSONObject();
                    putIfValid(row, Contract.RadioRow.PCI, id.getPci());
                    putIfValid(row, Contract.RadioRow.NCI, id.getNci());
                    putNrSignal(row, (CellSignalStrengthNr) ci.getCellSignalStrength());
                    return row;
                } else if (ci instanceof CellInfoLte && lte == null) {
                    CellIdentityLte id = ((CellInfoLte) ci).getCellIdentity();
                    CellSignalStrengthLte ss = ((CellInfoLte) ci).getCellSignalStrength();
                    lte = new JSONObject();
                    putIfValid(lte, Contract.RadioRow.PCI, id.getPci());
                    putIfValid(lte, Contract.RadioRow.RSRP, ss.getRsrp());
                    putIfValid(lte, Contract.RadioRow.RSRQ, ss.getRsrq());
                }
            }
            return lte;
        } catch (Exception e) {  // includes SecurityException
            return null;
        }
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
            row.put(Contract.RadioRow.MONO_NS, t[0]);
            row.put(Contract.RadioRow.UTC_NS, t[1]);
            boolean haveNr = false;
            boolean haveLte = false;
            // Unknown mode (no callback yet / old API): trust CellInfo as before.
            String mode = networkType;
            boolean nrActive = mode == null || mode.startsWith("NR");
            if (mode != null) {
                row.put(Contract.RadioRow.NETWORK_TYPE, mode);
            }

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
                    if ((primary || secondary) && nrActive && !haveNr) {
                        haveNr = true;
                        putIfValid(row, Contract.RadioRow.PCI, id.getPci());
                        putIfValid(row, Contract.RadioRow.NCI, id.getNci());
                        putIfValid(row, Contract.RadioRow.TAC, id.getTac());
                        putIfValid(row, Contract.RadioRow.NRARFCN, id.getNrarfcn());
                        putNrSignal(row, ss);
                        row.put(Contract.RadioRow.CELL_CONNECTION_STATUS, primary ? "PRIMARY" : "SECONDARY");
                    } else {
                        neighbors.put(neighbor(id.getPci(), ss.getSsRsrp()));
                    }
                } else if (ci instanceof CellInfoLte) {
                    CellInfoLte lte = (CellInfoLte) ci;
                    CellIdentityLte id = (CellIdentityLte) lte.getCellIdentity();
                    CellSignalStrengthLte ss = (CellSignalStrengthLte) lte.getCellSignalStrength();
                    if (ci.isRegistered() && !haveLte) {
                        haveLte = true;
                        putIfValid(row, Contract.RadioRow.LTE_PCI, id.getPci());
                        putIfValid(row, Contract.RadioRow.EARFCN, id.getEarfcn());
                        putIfValid(row, Contract.RadioRow.LTE_RSRP, ss.getRsrp());
                        if (valid(id.getBandwidth())) {
                            row.put(Contract.RadioRow.BANDWIDTH_MHZ, id.getBandwidth() / 1000);
                        }
                        putLteExtras(row, ss);
                    } else {
                        neighbors.put(neighbor(id.getPci(), ss.getRsrp()));
                    }
                }
            }
            // The NR leg's signal comes from SignalStrength, which the modem
            // keeps current. In NSA, CellInfoNr's signal is a snapshot from
            // when the 5G leg was added and never refreshes (one value for
            // hours), or is unavailable with a placeholder SINR of 0. Use
            // CellInfoNr's signal only when SignalStrength has none.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q && nrActive) {
                SignalStrength sig = tm.getSignalStrength();
                if (sig != null) {
                    for (CellSignalStrengthNr ss : sig.getCellSignalStrengths(CellSignalStrengthNr.class)) {
                        if (valid(ss.getSsRsrp())) {
                            row.remove(Contract.RadioRow.RSRP);
                            row.remove(Contract.RadioRow.RSRQ);
                            row.remove(Contract.RadioRow.SINR);
                            putNrSignal(row, ss);
                            if (!row.has(Contract.RadioRow.CELL_CONNECTION_STATUS)) {
                                row.put(Contract.RadioRow.CELL_CONNECTION_STATUS, haveLte ? "SECONDARY" : "PRIMARY");
                            }
                            break;
                        }
                    }
                }
            }
            // LTE extras (RSRQ, RS-SNR, CQI, timing advance) come from the
            // CellInfoLte above: requestCellInfoUpdate makes the modem measure
            // now. SignalStrength only changes on modem indications, which
            // Android switches off while the screen is off and the phone is
            // not charging (power saving): on the road it froze for a whole
            // ride while CellInfo kept changing. Fall back to it only for
            // fields CellInfo leaves unavailable.
            if (haveLte && Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                SignalStrength sig = tm.getSignalStrength();
                if (sig != null) {
                    for (CellSignalStrengthLte ss : sig.getCellSignalStrengths(CellSignalStrengthLte.class)) {
                        if (valid(ss.getRsrp())) {
                            putLteExtrasIfMissing(row, ss);
                            break;
                        }
                    }
                }
            }
            putCarrierAggregation(row, tm);
            if (neighbors.length() > 0) {
                row.put(Contract.RadioRow.NEIGHBOR_CELLS, neighbors);
            }
            putTransport(row);
            RustCore.pushRadio(handle, row.toString());
        } catch (Exception ignored) {
        }
    }

    private static void putLteExtrasIfMissing(JSONObject row, CellSignalStrengthLte ss) throws Exception {
        if (!row.has(Contract.RadioRow.LTE_RSRQ)) putIfValid(row, Contract.RadioRow.LTE_RSRQ, ss.getRsrq());
        if (!row.has(Contract.RadioRow.LTE_RSSNR)) putIfValid(row, Contract.RadioRow.LTE_RSSNR, ss.getRssnr());
        if (!row.has(Contract.RadioRow.LTE_CQI)) putIfValid(row, Contract.RadioRow.LTE_CQI, ss.getCqi());
        if (!row.has(Contract.RadioRow.LTE_TA)) putIfValid(row, Contract.RadioRow.LTE_TA, ss.getTimingAdvance());
    }

    private static void putLteExtras(JSONObject row, CellSignalStrengthLte ss) throws Exception {
        putIfValid(row, Contract.RadioRow.LTE_RSRQ, ss.getRsrq());
        putIfValid(row, Contract.RadioRow.LTE_RSSNR, ss.getRssnr());
        putIfValid(row, Contract.RadioRow.LTE_CQI, ss.getCqi());
        putIfValid(row, Contract.RadioRow.LTE_TA, ss.getTimingAdvance());
    }

    /** Bandwidth of every aggregated carrier (ServiceState, primary first),
     *  and the cellular network's link-capacity estimate — the modem's LCE
     *  when it reports one, otherwise the carrier's configured default. Both
     *  describe the cellular link even while data goes over Wi-Fi. */
    @SuppressWarnings({"MissingPermission", "deprecation"})
    private void putCarrierAggregation(JSONObject row, TelephonyManager tm) throws Exception {
        try {
            ServiceState st = tm.getServiceState();
            if (st != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                int[] bws = st.getCellBandwidths();
                if (bws != null && bws.length > 0) {
                    StringBuilder b = new StringBuilder();
                    for (int bw : bws) {
                        if (b.length() > 0) {
                            b.append(',');
                        }
                        b.append(bw);
                    }
                    row.put(Contract.RadioRow.CA_BANDWIDTHS_KHZ, b.toString());
                }
            }
        } catch (SecurityException ignored) {
        }
        ConnectivityManager cm = (ConnectivityManager) ctx.getSystemService(Context.CONNECTIVITY_SERVICE);
        for (Network n : cm.getAllNetworks()) {
            NetworkCapabilities caps = cm.getNetworkCapabilities(n);
            if (caps != null && caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR)
                    && caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)) {
                if (caps.getLinkDownstreamBandwidthKbps() > 0) {
                    row.put(Contract.RadioRow.LINK_DN_KBPS, caps.getLinkDownstreamBandwidthKbps());
                }
                if (caps.getLinkUpstreamBandwidthKbps() > 0) {
                    row.put(Contract.RadioRow.LINK_UP_KBPS, caps.getLinkUpstreamBandwidthKbps());
                }
                break;
            }
        }
    }

    /** Which network carries the data (the cellular fields describe the
     *  modem even while traffic goes over Wi-Fi), plus the Wi-Fi link when
     *  that is the one: frequency, channel, channel width, standard, RSSI and
     *  link rates. Channel width is not in WifiInfo; it comes from the
     *  cached scan entry of the connected access point. */
    @SuppressWarnings({"MissingPermission", "deprecation"})
    private void putTransport(JSONObject row) throws Exception {
        ConnectivityManager cm = (ConnectivityManager) ctx.getSystemService(Context.CONNECTIVITY_SERVICE);
        Network active = cm.getActiveNetwork();
        NetworkCapabilities caps = active == null ? null : cm.getNetworkCapabilities(active);
        if (caps == null) {
            row.put(Contract.RadioRow.TRANSPORT, "NONE");
            return;
        }
        boolean wifi = caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI);
        String transport = wifi ? "WIFI"
                : caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) ? "CELLULAR"
                : caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) ? "ETHERNET"
                : "OTHER";
        row.put(Contract.RadioRow.TRANSPORT, transport);
        if (!wifi) {
            return;
        }
        WifiManager wm = (WifiManager) ctx.getApplicationContext().getSystemService(Context.WIFI_SERVICE);
        WifiInfo info = wm.getConnectionInfo();
        if (info == null || info.getFrequency() <= 0) {
            return;
        }
        int freq = info.getFrequency();
        row.put(Contract.RadioRow.WIFI_FREQ_MHZ, freq);
        int channel = wifiChannel(freq);
        if (channel > 0) {
            row.put(Contract.RadioRow.WIFI_CHANNEL, channel);
        }
        if (info.getRssi() > -127) {
            row.put(Contract.RadioRow.WIFI_RSSI, info.getRssi());
        }
        if (info.getTxLinkSpeedMbps() > 0) {
            row.put(Contract.RadioRow.WIFI_TX_MBPS, info.getTxLinkSpeedMbps());
        }
        if (info.getRxLinkSpeedMbps() > 0) {
            row.put(Contract.RadioRow.WIFI_RX_MBPS, info.getRxLinkSpeedMbps());
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            String std = wifiStandard(info.getWifiStandard());
            if (std != null) {
                row.put(Contract.RadioRow.WIFI_STANDARD, std);
            }
        }
        String bssid = info.getBSSID();
        try {
            for (ScanResult sr : wm.getScanResults()) {
                if (bssid != null && bssid.equalsIgnoreCase(sr.BSSID)) {
                    int bw = channelWidthMhz(sr.channelWidth);
                    if (bw > 0) {
                        row.put(Contract.RadioRow.WIFI_BW_MHZ, bw);
                    }
                    break;
                }
            }
        } catch (SecurityException ignored) {
            // no location permission: width stays unknown
        }
    }

    static int wifiChannel(int freq) {
        if (freq == 2484) return 14;
        if (freq >= 2412 && freq <= 2472) return (freq - 2407) / 5;
        if (freq >= 5955 && freq <= 7115) return (freq - 5950) / 5;  // 6 GHz
        if (freq >= 5160 && freq <= 5885) return (freq - 5000) / 5;
        return -1;
    }

    static int channelWidthMhz(int width) {
        switch (width) {
            case ScanResult.CHANNEL_WIDTH_20MHZ: return 20;
            case ScanResult.CHANNEL_WIDTH_40MHZ: return 40;
            case ScanResult.CHANNEL_WIDTH_80MHZ: return 80;
            case ScanResult.CHANNEL_WIDTH_160MHZ:
            case ScanResult.CHANNEL_WIDTH_80MHZ_PLUS_MHZ: return 160;
            case 5: return 320;  // ScanResult.CHANNEL_WIDTH_320MHZ (API 33)
            default: return -1;
        }
    }

    static String wifiStandard(int std) {
        switch (std) {
            case 1: return "802.11a/b/g";  // ScanResult.WIFI_STANDARD_LEGACY
            case 4: return "802.11n";
            case 5: return "802.11ac";
            case 6: return "802.11ax";
            case 7: return "802.11ad";
            case 8: return "802.11be";
            default: return null;
        }
    }

    /** Telephony reports "not available" as CellInfo.UNAVAILABLE
     *  (Integer.MAX_VALUE) / UNAVAILABLE_LONG; those must be null, not dBm. */
    private static boolean valid(long v) {
        return v != CellInfo.UNAVAILABLE && v != CellInfo.UNAVAILABLE_LONG;
    }

    /** NR signal fields; SINR is only trusted beside a valid RSRP (Pixel
     *  reports SINR 0, not UNAVAILABLE, when it has no measurement). */
    private static void putNrSignal(JSONObject row, CellSignalStrengthNr ss) throws Exception {
        if (!valid(ss.getSsRsrp())) {
            return;
        }
        row.put(Contract.RadioRow.RSRP, ss.getSsRsrp());
        putIfValid(row, Contract.RadioRow.RSRQ, ss.getSsRsrq());
        putIfValid(row, Contract.RadioRow.SINR, ss.getSsSinr());
    }

    private static void putIfValid(JSONObject row, String key, long v) throws Exception {
        if (valid(v)) {
            row.put(key, v);
        }
    }

    private static JSONObject neighbor(int pci, int rsrp) throws Exception {
        JSONObject n = new JSONObject();
        n.put(Contract.RadioRow.NeighborCells.PCI, pci);
        putIfValid(n, Contract.RadioRow.NeighborCells.RSRP, rsrp);
        return n;
    }
}
