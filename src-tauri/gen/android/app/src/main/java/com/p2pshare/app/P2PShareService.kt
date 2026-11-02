package com.p2pshare.app

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat

/**
 * Keeps the process, and so the P2P core in it, alive once the activity is
 * gone: friends and LocalSend devices still reach the phone, like the
 * desktop app in the tray. Turned off in the settings, the app only works
 * while open.
 *
 * Nothing to do here but be there: the Rust core runs in this process and
 * keeps its own connections (no wake lock nor alarm; a foreground service
 * keeps the network during Doze).
 */
class P2PShareService : Service() {
  companion object {
    private const val TAG = "P2PShareService"
    const val CHANNEL_SERVICE = "service"
    const val CHANNEL_EVENTS = "events"
    const val NOTIFICATION_ID = 1
    private const val PREFS = "p2pshare"
    private const val KEY_ENABLED = "backgroundService"

    fun enabled(context: Context): Boolean =
      context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean(KEY_ENABLED, true)

    fun setEnabled(context: Context, enabled: Boolean) {
      context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
        .putBoolean(KEY_ENABLED, enabled).apply()
      if (enabled) start(context) else context.stopService(Intent(context, P2PShareService::class.java))
    }

    /** Only from the foreground (Android 12+ refuses it from the background). */
    fun start(context: Context) {
      if (!enabled(context)) return
      try {
        ContextCompat.startForegroundService(context, Intent(context, P2PShareService::class.java))
      } catch (e: Exception) {
        Log.w(TAG, "not started: $e")
      }
    }

    fun createChannels(context: Context) {
      if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
      val manager = context.getSystemService(NotificationManager::class.java) ?: return
      manager.createNotificationChannel(
        NotificationChannel(CHANNEL_SERVICE, "Fonctionnement en arrière-plan", NotificationManager.IMPORTANCE_MIN).apply {
          description = "Indique que P2P Share reste joignable quand l'application est fermée."
          setShowBadge(false)
        }
      )
      manager.createNotificationChannel(
        NotificationChannel(CHANNEL_EVENTS, "Messages et fichiers", NotificationManager.IMPORTANCE_HIGH).apply {
          description = "Messages, fichiers reçus et activité des groupes."
        }
      )
    }
  }

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    createChannels(this)
    val open = PendingIntent.getActivity(
      this, 0,
      Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    )
    val notification = NotificationCompat.Builder(this, CHANNEL_SERVICE)
      .setSmallIcon(R.drawable.ic_notification)
      .setContentTitle("P2P Share est actif")
      .setContentText("Vos amis et les appareils LocalSend peuvent vous joindre.")
      .setContentIntent(open)
      .setOngoing(true)
      .setShowWhen(false)
      .setPriority(NotificationCompat.PRIORITY_MIN)
      .setCategory(NotificationCompat.CATEGORY_SERVICE)
      .build()
    val type = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
      ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE
    } else {
      0
    }
    try {
      ServiceCompat.startForeground(this, NOTIFICATION_ID, notification, type)
    } catch (e: Exception) {
      Log.w(TAG, "not in the foreground: $e")
      stopSelf()
    }
    // Not restarted alone after the process is killed: the core only runs
    // with the app, which starts the service again.
    return START_NOT_STICKY
  }
}
