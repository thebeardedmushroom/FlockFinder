package org.flockfinder.app

import android.app.Activity
import android.os.Bundle
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge
import java.lang.ref.WeakReference

class MainActivity : TauriActivity() {
  private var hasWebView = false

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    current = WeakReference(this)
  }

  override fun onWebViewCreate(webView: WebView) {
    hasWebView = true
  }

  override fun onResume() {
    super.onResume()
    // Android recreated this activity while the process lived on (navigation's service kept
    // it): Tauri gives it no webview, so ask for the window to be built again (src-tauri/src/lib.rs).
    if (!hasWebView) windowNeeded()
  }

  override fun onDestroy() {
    if (current?.get() === this) current = null
    super.onDestroy()
  }

  private external fun windowNeeded()

  companion object {
    /**
     * The live activity. Android may destroy and recreate it while the process (and a
     * navigation session) carries on, but Tauri plugins keep the activity they were created
     * with, so anything acting on the window asks here.
     */
    @Volatile private var current: WeakReference<Activity>? = null

    fun current(): Activity? = current?.get()
  }
}
