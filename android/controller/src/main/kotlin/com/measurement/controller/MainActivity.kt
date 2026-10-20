package com.measurement.controller

import android.content.Context
import android.content.SharedPreferences
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.widget.ArrayAdapter
import android.widget.AutoCompleteTextView
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import com.measurement.controller.client.api.OperatorApi
import com.measurement.controller.client.api.OperatorAuthApi
import com.measurement.controller.client.model.AppProfile
import com.measurement.controller.client.model.CreateTestRequest
import com.measurement.controller.client.model.DeviceSummary
import com.measurement.controller.client.model.Direction
import com.measurement.controller.client.model.Flow
import com.measurement.controller.client.model.LoginRequest
import com.measurement.controller.client.model.TestPlan
import com.measurement.controller.client.model.TestSummary
import okhttp3.Interceptor
import okhttp3.OkHttpClient

/**
 * Operator console (Phase 3b/3c). Logs in against server-main, lists the fleet +
 * recent tests, starts preset tests on the selected device, and stops a running
 * test — all via the OpenAPI-generated client. No measurement, no core.
 */
class MainActivity : AppCompatActivity() {

    private var jwt: String? = null
    private var baseUrl: String = ""
    private var deviceId: String? = null
    private var lastTests: List<TestSummary> = emptyList()
    @Volatile private var refreshing = false
    private var authedClient: OkHttpClient? = null

    private val ui = Handler(Looper.getMainLooper())
    private val refresh = object : Runnable {
        override fun run() {
            jwt?.let { token -> runBg { refreshFleet(token) } } // network off the main thread
            ui.postDelayed(this, 3000)
        }
    }

    private lateinit var status: TextView
    private lateinit var output: TextView
    private lateinit var startProbe: Button
    private lateinit var startRtUp: Button
    private lateinit var startRtDown: Button
    private lateinit var startBulkUp: Button
    private lateinit var startBulkDown: Button
    private lateinit var startFull: Button
    private lateinit var stopBtn: Button
    private val startButtons get() =
        listOf(startProbe, startRtUp, startRtDown, startBulkUp, startBulkDown, startFull)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        val serverUrl = findViewById<AutoCompleteTextView>(R.id.serverUrl)
        val username = findViewById<EditText>(R.id.username)
        val password = findViewById<EditText>(R.id.password)
        status = findViewById(R.id.status)
        output = findViewById(R.id.output)
        startProbe = findViewById(R.id.startProbeBtn)
        startRtUp = findViewById(R.id.startRtUpBtn)
        startRtDown = findViewById(R.id.startRtDownBtn)
        startBulkUp = findViewById(R.id.startBulkUpBtn)
        startBulkDown = findViewById(R.id.startBulkDownBtn)
        startFull = findViewById(R.id.startFullBtn)
        stopBtn = findViewById(R.id.stopBtn)
        val refreshBtn = findViewById<Button>(R.id.refreshBtn)

        // server-URL history: persisted in SharedPreferences (newline-joined,
        // most-recently-used first); the dropdown shows it on focus/click and
        // any typed URL that gets used is remembered (see rememberUrl).
        val prefs = getSharedPreferences("mp_controller", Context.MODE_PRIVATE)
        val urls = loadUrls(prefs).toMutableList()
        val adapter = ArrayAdapter(this, android.R.layout.simple_dropdown_item_1line, urls)
        serverUrl.setAdapter(adapter)
        serverUrl.threshold = 0
        serverUrl.setOnFocusChangeListener { _, hasFocus ->
            if (hasFocus && urls.isNotEmpty()) serverUrl.showDropDown()
        }
        serverUrl.setOnClickListener { if (urls.isNotEmpty()) serverUrl.showDropDown() }
        if (urls.isNotEmpty() && serverUrl.text.isEmpty()) serverUrl.setText(urls[0])

        findViewById<Button>(R.id.loginBtn).setOnClickListener {
            baseUrl = serverUrl.text.toString().trim().trimEnd('/')
            if (baseUrl.isEmpty()) { status.text = "Enter a server URL"; return@setOnClickListener }
            rememberUrl(prefs, urls, adapter, baseUrl)
            val user = username.text.toString().trim()
            val pass = password.text.toString()
            status.text = "Logging in…"
            runBg {
                try {
                    val resp = OperatorAuthApi(baseUrl).login(LoginRequest(password = pass, username = user))
                    jwt = resp.jwt
                    ui {
                        status.text = "Logged in as $user (${resp.role.value})"
                        refreshBtn.isEnabled = true
                        ui.removeCallbacks(refresh)
                        ui.post(refresh) // start live polling
                    }
                } catch (e: Exception) {
                    ui { status.text = "Login failed: ${e.message}" }
                }
            }
        }

        refreshBtn.setOnClickListener { jwt?.let { runBg { refreshFleet(it) } } }
        startProbe.setOnClickListener   { startTest(probePlan(),     "probe") }
        startRtUp.setOnClickListener    { startTest(rtUpPlan(),      "rt-up") }
        startRtDown.setOnClickListener  { startTest(rtDownPlan(),    "rt-down") }
        startBulkUp.setOnClickListener  { startTest(bulkUpPlan(),    "bulk-up") }
        startBulkDown.setOnClickListener{ startTest(bulkDownPlan(),  "bulk-down") }
        startFull.setOnClickListener    { startTest(fullPlan(),      "full") }
        stopBtn.setOnClickListener { stopActiveTest() }
    }

    override fun onDestroy() {
        super.onDestroy()
        ui.removeCallbacks(refresh)
    }

    // ---- networking ----

    /** One bearer-authed client, reused (the spec has no security scheme, so the
     *  token is attached via an interceptor). */
    private fun operatorApi(token: String): OperatorApi {
        val client = authedClient ?: OkHttpClient.Builder()
            .addInterceptor(Interceptor { chain ->
                chain.proceed(
                    chain.request().newBuilder().header("Authorization", "Bearer $token").build()
                )
            })
            .build()
            .also { authedClient = it }
        return OperatorApi(baseUrl, client)
    }

    /** Fetch + render fleet/tests; runs on a background thread. Skips if a
     *  refresh is already in flight so periodic + manual ticks don't overlap. */
    private fun refreshFleet(token: String) {
        if (refreshing) return
        refreshing = true
        try {
            val op = operatorApi(token)
            val devices = op.listDevices()
            val tests = op.listTests()
            ui {
                // Pick the most-recently-heartbeating device (live one), not
                // simply the first enrolled — the latter can be a stale ghost.
                if (deviceId == null) {
                    deviceId = devices.maxByOrNull { it.lastSeenUtcNs ?: 0L }?.deviceId
                }
                lastTests = tests
                val canControl = deviceId != null
                startButtons.forEach { it.isEnabled = canControl }
                stopBtn.isEnabled = tests.any { it.endedUtcNs == null }
                val active = tests.count { it.endedUtcNs == null }
                status.text = "${devices.size} device(s) · ${tests.size} tests · $active active"
                output.text = render(devices, tests)
            }
        } catch (e: Exception) {
            ui { status.text = "Refresh failed: ${e.message ?: e.javaClass.simpleName}" }
        } finally {
            refreshing = false
        }
    }

    private fun startTest(plan: TestPlan, label: String) {
        val token = jwt ?: return
        val dev = deviceId ?: run { status.text = "No device"; return }
        status.text = "Starting $label on $dev…"
        runBg {
            try {
                val t = operatorApi(token).createTest(CreateTestRequest(deviceId = dev, plan = plan))
                ui { status.text = "Started ${t.planName} → ${t.state.value}" }
                refreshFleet(token) // already on the bg thread of startTest
            } catch (e: Exception) {
                ui { status.text = "Start failed: ${e.message}" }
            }
        }
    }

    private fun stopActiveTest() {
        val token = jwt ?: return
        val active = lastTests.firstOrNull { it.endedUtcNs == null } ?: run {
            status.text = "No active test"; return
        }
        status.text = "Stopping ${active.planName}…"
        runBg {
            try {
                val t = operatorApi(token).stopTest(active.testId)
                ui { status.text = "Stopped → ${t.state.value}/${t.result?.value}" }
                refreshFleet(token) // already on the bg thread of stopActiveTest
            } catch (e: Exception) {
                ui { status.text = "Stop failed: ${e.message}" }
            }
        }
    }

    // ---- preset plans (flattened Flow: fill all fields; server reads per type) ----

    // The generated client flattens the Flow union into one class, so fields
    // of other flow types are required too: direction in `probe` flows,
    // ratePps/packetSizeBytes for `bulk_tcp`, dscpValues (dscp_probe) and
    // profile (app_traffic) everywhere. The server ignores fields that don't
    // belong to the flow's type; `flow` fills the latter two.
    private fun flow(
        flowId: Int, type: Flow.Type, ratePps: Int, packetSizeBytes: Int, direction: Direction,
        targetBytes: Int? = null,
    ) = Flow(
        flowId = flowId, type = type, ratePps = ratePps, packetSizeBytes = packetSizeBytes,
        direction = direction, targetBytes = targetBytes, dscpValues = emptyList(), profile = AppProfile.web,
    )

    private fun probePlan() = TestPlan(
        durationSec = 30, name = "ctrl-probe", version = 1,
        flows = listOf(
            flow(flowId = 1, type = Flow.Type.probe, ratePps = 50, packetSizeBytes = 22, direction = Direction.ToServer)
        )
    )

    private fun rtUpPlan() = TestPlan(
        durationSec = 30, name = "ctrl-rt-up", version = 1,
        flows = listOf(
            flow(flowId = 1, type = Flow.Type.realtime_udp, ratePps = 100, packetSizeBytes = 200, direction = Direction.ToServer)
        )
    )

    private fun rtDownPlan() = TestPlan(
        durationSec = 30, name = "ctrl-rt-down", version = 1,
        flows = listOf(
            flow(flowId = 1, type = Flow.Type.realtime_udp, ratePps = 100, packetSizeBytes = 200, direction = Direction.FromServer)
        )
    )

    private fun bulkUpPlan() = TestPlan(
        durationSec = 30, name = "ctrl-bulk-up", version = 1,
        flows = listOf(
            flow(flowId = 1, type = Flow.Type.bulk_tcp, ratePps = 0, packetSizeBytes = 0, direction = Direction.ToServer, targetBytes = 5_000_000)
        )
    )

    private fun bulkDownPlan() = TestPlan(
        durationSec = 30, name = "ctrl-bulk-down", version = 1,
        flows = listOf(
            flow(flowId = 1, type = Flow.Type.bulk_tcp, ratePps = 0, packetSizeBytes = 0, direction = Direction.FromServer, targetBytes = 5_000_000)
        )
    )

    /** Everything: RTT (probe), OWD both ways (realtime UDP), throughput both
     *  ways (bulk TCP). One comprehensive 30-second snapshot. */
    private fun fullPlan() = TestPlan(
        durationSec = 30, name = "ctrl-full", version = 1,
        flows = listOf(
            flow(flowId = 1, type = Flow.Type.probe, ratePps = 50, packetSizeBytes = 22, direction = Direction.ToServer),
            flow(flowId = 2, type = Flow.Type.realtime_udp, ratePps = 100, packetSizeBytes = 200, direction = Direction.ToServer),
            flow(flowId = 3, type = Flow.Type.realtime_udp, ratePps = 100, packetSizeBytes = 200, direction = Direction.FromServer),
            flow(flowId = 4, type = Flow.Type.bulk_tcp, ratePps = 0, packetSizeBytes = 0, direction = Direction.ToServer, targetBytes = 3_000_000),
            flow(flowId = 5, type = Flow.Type.bulk_tcp, ratePps = 0, packetSizeBytes = 0, direction = Direction.FromServer, targetBytes = 3_000_000)
        )
    )

    // ---- rendering ----

    private fun render(devices: List<DeviceSummary>, tests: List<TestSummary>): String {
        val sb = StringBuilder()
        sb.append("DEVICES (${devices.size})\n")
        for (d in devices) {
            val sel = if (d.deviceId == deviceId) "▶ " else "  "
            sb.append("$sel${d.model ?: d.platform}  [${d.status}]  state=${d.loggerState ?: "—"}\n")
        }
        sb.append("\nRECENT TESTS (${tests.size})\n")
        for (t in tests.take(15)) {
            val live = if (t.endedUtcNs == null) "● " else "  "
            sb.append("$live${t.planName}  ${t.state.value}${t.result?.let { " / ${it.value}" } ?: ""}\n")
        }
        return sb.toString()
    }

    private fun runBg(block: () -> Unit) = Thread(block).start()
    private fun ui(block: () -> Unit) = runOnUiThread(block)

    // ---- server-URL history (persisted in SharedPreferences) ----

    private fun loadUrls(prefs: SharedPreferences): List<String> =
        prefs.getString("server_urls", "")!!.split('\n').filter { it.isNotEmpty() }

    /** Move-to-top + de-dup + cap at 10; persist immediately. */
    private fun rememberUrl(
        prefs: SharedPreferences,
        urls: MutableList<String>,
        adapter: ArrayAdapter<String>,
        url: String
    ) {
        urls.remove(url)
        urls.add(0, url)
        while (urls.size > 10) urls.removeAt(urls.size - 1)
        prefs.edit().putString("server_urls", urls.joinToString("\n")).apply()
        adapter.notifyDataSetChanged()
    }
}
