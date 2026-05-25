package com.measurement.controller

import android.os.Bundle
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import com.measurement.controller.client.api.OperatorApi
import com.measurement.controller.client.api.OperatorAuthApi
import com.measurement.controller.client.model.DeviceSummary
import com.measurement.controller.client.model.LoginRequest
import com.measurement.controller.client.model.TestSummary
import okhttp3.Interceptor
import okhttp3.OkHttpClient

/**
 * Minimal operator console (Phase 3b). Logs in against server-main and lists the
 * fleet + recent tests via the generated OpenAPI client. No measurement, no core.
 */
class MainActivity : AppCompatActivity() {

    private var jwt: String? = null
    private var baseUrl: String = ""

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        val serverUrl = findViewById<EditText>(R.id.serverUrl)
        val username = findViewById<EditText>(R.id.username)
        val password = findViewById<EditText>(R.id.password)
        val status = findViewById<TextView>(R.id.status)
        val output = findViewById<TextView>(R.id.output)
        val refreshBtn = findViewById<Button>(R.id.refreshBtn)

        findViewById<Button>(R.id.loginBtn).setOnClickListener {
            baseUrl = serverUrl.text.toString().trim().trimEnd('/')
            val user = username.text.toString().trim()
            val pass = password.text.toString()
            status.text = "Logging in…"
            runBg {
                try {
                    val resp = OperatorAuthApi(baseUrl).login(LoginRequest(password = pass, username = user))
                    jwt = resp.jwt
                    val (devices, tests) = fetchFleet(resp.jwt)
                    ui {
                        status.text = "Logged in as $user (${resp.role}) — ${devices.size} devices"
                        refreshBtn.isEnabled = true
                        output.text = render(devices, tests)
                    }
                } catch (e: Exception) {
                    ui { status.text = "Login failed: ${e.message}" }
                }
            }
        }

        refreshBtn.setOnClickListener {
            val token = jwt ?: return@setOnClickListener
            status.text = "Refreshing…"
            runBg {
                try {
                    val (devices, tests) = fetchFleet(token)
                    ui {
                        status.text = "Refreshed — ${devices.size} devices"
                        output.text = render(devices, tests)
                    }
                } catch (e: Exception) {
                    ui { status.text = "Refresh failed: ${e.message}" }
                }
            }
        }
    }

    /** Bearer-authed client for operator endpoints (the spec has no security
     *  scheme, so the token is attached via an interceptor). */
    private fun authedClient(token: String): OkHttpClient =
        OkHttpClient.Builder()
            .addInterceptor(Interceptor { chain ->
                chain.proceed(
                    chain.request().newBuilder()
                        .header("Authorization", "Bearer $token")
                        .build()
                )
            })
            .build()

    private fun fetchFleet(token: String): Pair<List<DeviceSummary>, List<TestSummary>> {
        val client = authedClient(token)
        val op = OperatorApi(baseUrl, client)
        return op.listDevices() to op.listTests()
    }

    private fun render(devices: List<DeviceSummary>, tests: List<TestSummary>): String {
        val sb = StringBuilder()
        sb.append("DEVICES (${devices.size})\n")
        for (d in devices) {
            sb.append("• ${d.model ?: d.platform}  [${d.status}]\n")
            sb.append("    ${d.deviceId}\n")
            sb.append("    state=${d.loggerState ?: "—"}  batt=${d.batteryLevelPct ?: "—"}\n")
        }
        sb.append("\nRECENT TESTS (${tests.size})\n")
        for (t in tests.take(15)) {
            sb.append("• ${t.planName}  ${t.state}${t.result?.let { " / $it" } ?: ""}\n")
            sb.append("    ${t.testId}\n")
        }
        return sb.toString()
    }

    private fun runBg(block: () -> Unit) = Thread(block).start()
    private fun ui(block: () -> Unit) = runOnUiThread(block)
}
