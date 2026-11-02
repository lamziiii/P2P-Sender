package com.p2pshare.app

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.enableEdgeToEdge
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    P2PShareService.createChannels(this)
    askPermissions()
    P2PSharePlugin.handleIntent(intent)
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    setIntent(intent)
    P2PSharePlugin.handleIntent(intent)
  }

  override fun onResume() {
    super.onResume()
    P2PSharePlugin.setForeground(true)
    // From the foreground only: Android 12+ refuses it from the background.
    P2PShareService.start(this)
  }

  override fun onPause() {
    super.onPause()
    P2PSharePlugin.setForeground(false)
  }

  /** Notifications (Android 13+), and Download/P2PShare on Android 9 and 10. */
  private fun askPermissions() {
    val wanted = ArrayList<String>()
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
      wanted.add(Manifest.permission.POST_NOTIFICATIONS)
    }
    if (Build.VERSION.SDK_INT <= Build.VERSION_CODES.Q) {
      wanted.add(Manifest.permission.WRITE_EXTERNAL_STORAGE)
    }
    val missing = wanted.filter {
      ContextCompat.checkSelfPermission(this, it) != PackageManager.PERMISSION_GRANTED
    }
    if (missing.isNotEmpty()) {
      ActivityCompat.requestPermissions(this, missing.toTypedArray(), 1)
    }
  }
}
