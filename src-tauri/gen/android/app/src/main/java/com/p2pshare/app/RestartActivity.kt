package com.p2pshare.app

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import android.os.Process

/**
 * Runs in a process of its own (":restart"): once the app's process is
 * gone, starts the app again, then ends. Settings read at start (relay, a
 * restored backup) apply this way.
 */
class RestartActivity : Activity() {
  companion object {
    const val EXTRA_PID = "com.p2pshare.app.PID"
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    val pid = intent.getIntExtra(EXTRA_PID, -1)
    if (pid > 0 && pid != Process.myPid()) Process.killProcess(pid)
    startActivity(
      Intent(this, MainActivity::class.java)
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TASK)
    )
    finish()
    Runtime.getRuntime().exit(0)
  }
}
