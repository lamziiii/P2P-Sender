package com.p2pshare.app

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.app.DownloadManager
import android.app.PendingIntent
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Environment
import android.os.PowerManager
import android.os.Process
import android.provider.OpenableColumns
import android.provider.Settings
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import android.webkit.MimeTypeMap
import android.widget.Toast
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.core.content.FileProvider
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Channel
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.security.KeyStore
import java.util.UUID
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicInteger
import javax.crypto.AEADBadTagException
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import org.json.JSONArray
import org.json.JSONObject

@InvokeArg
class ListenArgs {
  lateinit var channel: Channel
}

@InvokeArg
class EnabledArgs {
  var enabled: Boolean = false
}

@InvokeArg
class NotifyArgs {
  lateinit var title: String
  lateinit var body: String
  var kind: String? = null
  var target: String? = null
}

@InvokeArg
class ImportArgs {
  lateinit var uri: String
  var keep: Boolean = false
}

@InvokeArg
class ExportArgs {
  lateinit var path: String
  lateinit var uri: String
}

@InvokeArg
class PathArgs {
  lateinit var path: String
}

@InvokeArg
class UrlArgs {
  lateinit var url: String
}

@InvokeArg
class LangArgs {
  lateinit var lang: String
}

@InvokeArg
class DataArgs {
  lateinit var data: String
}

/**
 * What the Rust side (src-tauri/src/android.rs) asks of Android. Commands run
 * on the UI thread: long ones answer from a thread of their own. Only the
 * application context is kept, the activity may be gone and come back.
 */
@TauriPlugin
class P2PSharePlugin(activity: Activity) : Plugin(activity) {
  private val context: Context = activity.applicationContext

  companion object {
    const val EXTRA_KIND = "com.p2pshare.app.KIND"
    const val EXTRA_ID = "com.p2pshare.app.ID"
    private const val KEY_ALIAS = "p2pshare-vault"
    private const val GCM_IV = 12
    private const val GCM_TAG_BITS = 128
    /** Days a copy of a sent document is kept in the cache. */
    private const val SENT_COPIES_DAYS = 7L

    private val io = Executors.newCachedThreadPool()
    private val lock = Any()
    private var listener: Channel? = null
    /** Events the Rust side has not heard yet (sent once it listens). */
    private val pending = ArrayList<JSObject>()
    private var multicast: WifiManager.MulticastLock? = null
    /** Held while files move (see setTransferring). */
    private var wifiLock: WifiManager.WifiLock? = null
    private var wakeLock: PowerManager.WakeLock? = null
    private val notificationIds = AtomicInteger(1000)

    private fun send(event: JSObject) {
      synchronized(lock) {
        val l = listener
        if (l == null) {
          pending.add(event)
        } else {
          l.send(event)
        }
      }
    }

    /**
     * A notification about a chat was tapped: open that chat. Or another
     * app shared files or a text: the window asks where they go.
     */
    fun handleIntent(context: Context, intent: Intent?) {
      if (intent?.action == Intent.ACTION_SEND || intent?.action == Intent.ACTION_SEND_MULTIPLE) {
        shared(context, intent)
        return
      }
      val kind = intent?.getStringExtra(EXTRA_KIND) ?: return
      val id = intent.getStringExtra(EXTRA_ID) ?: return
      intent.removeExtra(EXTRA_KIND)
      intent.removeExtra(EXTRA_ID)
      send(JSObject().apply {
        put("kind", kind)
        put("id", id)
      })
    }

    /** Documents shared (their content:// address and name), else the text. */
    @Suppress("DEPRECATION")
    private fun shared(context: Context, intent: Intent) {
      val uris = ArrayList<Uri>()
      if (intent.action == Intent.ACTION_SEND_MULTIPLE) {
        intent.getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM)?.let { uris.addAll(it) }
      } else {
        intent.getParcelableExtra<Uri>(Intent.EXTRA_STREAM)?.let { uris.add(it) }
      }
      // Some apps only give them in the clip.
      if (uris.isEmpty()) {
        val clip = intent.clipData
        if (clip != null) for (i in 0 until clip.itemCount) clip.getItemAt(i).uri?.let { uris.add(it) }
      }
      // Read with the app's own rights: only another app's documents, never
      // a file path or our own provider (they would reach the app's files).
      uris.retainAll { it.scheme == "content" && !ownAuthority(context, it.authority) }
      val items = JSONArray()
      for (uri in uris) {
        items.put(JSONObject().apply {
          put("uri", uri.toString())
          put("name", nameOf(context, uri) ?: "fichier")
        })
      }
      // Handled once, not again when the activity comes back.
      intent.action = Intent.ACTION_MAIN
      send(JSObject().apply {
        put("shared", items)
        if (uris.isEmpty()) intent.getStringExtra(Intent.EXTRA_TEXT)?.let { put("text", it) }
      })
    }

    /** One of the app's own content providers (FileProvider and the like). */
    private fun ownAuthority(context: Context, authority: String?): Boolean {
      if (authority == null) return true
      return authority.split(';').any { it == context.packageName || it.startsWith("${context.packageName}.") }
    }

    fun nameOf(context: Context, uri: Uri): String? {
      try {
        context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
          if (c.moveToFirst() && !c.isNull(0)) return c.getString(0)
        }
      } catch (_: Exception) {
      }
      return uri.lastPathSegment?.substringAfterLast('/')
    }

    /** The activity is on screen, or no longer: no notification for what is seen. */
    fun setForeground(foreground: Boolean) {
      send(JSObject().apply { put("foreground", foreground) })
    }
  }

  @Command
  fun listen(invoke: Invoke) {
    val args = invoke.parseArgs(ListenArgs::class.java)
    synchronized(lock) {
      listener = args.channel
      for (event in pending) args.channel.send(event)
      pending.clear()
    }
    invoke.resolve()
  }

  @Command
  fun setService(invoke: Invoke) {
    val args = invoke.parseArgs(EnabledArgs::class.java)
    P2PShareService.setEnabled(context, args.enabled)
    invoke.resolve()
  }

  @SuppressLint("MissingPermission")
  @Command
  fun notify(invoke: Invoke) {
    val args = invoke.parseArgs(NotifyArgs::class.java)
    invoke.resolve()
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
      ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
    ) {
      return
    }
    P2PShareService.createChannels(context)
    val kind = args.kind
    val target = args.target
    // One per chat, the latest replacing the one before.
    val id = if (kind != null && target != null) {
      "$kind:$target".hashCode().let { if (it == P2PShareService.NOTIFICATION_ID) it + 1 else it }
    } else {
      notificationIds.incrementAndGet()
    }
    val open = Intent(context, MainActivity::class.java).apply {
      addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
      if (kind != null && target != null) {
        putExtra(EXTRA_KIND, kind)
        putExtra(EXTRA_ID, target)
      }
    }
    val tap = PendingIntent.getActivity(
      context, id, open, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    )
    val notification = NotificationCompat.Builder(context, P2PShareService.CHANNEL_EVENTS)
      .setSmallIcon(R.drawable.ic_notification)
      .setContentTitle(args.title)
      .setContentText(args.body)
      .setStyle(NotificationCompat.BigTextStyle().bigText(args.body))
      .setContentIntent(tap)
      .setAutoCancel(true)
      .setPriority(NotificationCompat.PRIORITY_HIGH)
      .setCategory(NotificationCompat.CATEGORY_MESSAGE)
      .build()
    try {
      NotificationManagerCompat.from(context).notify(id, notification)
    } catch (e: SecurityException) {
      // Permission taken back meanwhile.
    }
  }

  /** LocalSend is always on: the lock is kept for the life of the app. */
  @Command
  fun holdMulticast(invoke: Invoke) {
    synchronized(lock) {
      if (multicast == null) {
        val wifi = context.getSystemService(Context.WIFI_SERVICE) as? WifiManager
        multicast = wifi?.createMulticastLock("p2pshare-localsend")?.apply {
          setReferenceCounted(false)
          acquire()
        }
      }
    }
    invoke.resolve()
  }

  /**
   * While files move, Android must not slow the Wi-Fi down or put the
   * processor to sleep, as it does to save battery (screen off above all).
   */
  @Suppress("DEPRECATION", "WakelockTimeout")
  @Command
  fun setTransferring(invoke: Invoke) {
    val args = invoke.parseArgs(EnabledArgs::class.java)
    synchronized(lock) {
      if (args.enabled) {
        if (wifiLock == null) {
          val wifi = context.getSystemService(Context.WIFI_SERVICE) as? WifiManager
          // High performance is gone from Android 14: low latency took its place.
          val mode = if (Build.VERSION.SDK_INT >= 34) WifiManager.WIFI_MODE_FULL_LOW_LATENCY else WifiManager.WIFI_MODE_FULL_HIGH_PERF
          wifiLock = wifi?.createWifiLock(mode, "p2pshare-transfer")?.apply {
            setReferenceCounted(false)
            acquire()
          }
        }
        if (wakeLock == null) {
          val power = context.getSystemService(Context.POWER_SERVICE) as? PowerManager
          wakeLock = power?.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "p2pshare:transfer")?.apply {
            setReferenceCounted(false)
            acquire()
          }
        }
      } else {
        wifiLock?.takeIf { it.isHeld }?.release()
        wifiLock = null
        wakeLock?.takeIf { it.isHeld }?.release()
        wakeLock = null
      }
    }
    invoke.resolve()
  }

  @Command
  fun downloadDirs(invoke: Invoke) {
    @Suppress("DEPRECATION")
    val public = File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), "P2PShare")
    val fallback = File(
      context.getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS) ?: File(context.filesDir, "downloads"),
      "P2PShare"
    )
    invoke.resolve(JSObject().apply {
      put("public", public.absolutePath)
      put("fallback", fallback.absolutePath)
    })
  }

  private fun displayName(uri: Uri): String? = nameOf(context, uri)

  private fun safeName(name: String?): String {
    val clean = (name ?: "").replace(Regex("[/\\\\\u0000-\u001f]"), "_").trim().trimStart('.')
    return clean.take(200).ifEmpty { "fichier" }
  }

  /** Copies of sent documents older than a week go (the cache may go sooner). */
  private fun prune(base: File) {
    val limit = System.currentTimeMillis() - SENT_COPIES_DAYS * 24 * 3600 * 1000
    base.listFiles()?.forEach { if (it.lastModified() < limit) it.deleteRecursively() }
  }

  /** A document picked through the system, copied into the app: its path. */
  @Command
  fun importUri(invoke: Invoke) {
    val args = invoke.parseArgs(ImportArgs::class.java)
    io.execute {
      try {
        val uri = Uri.parse(args.uri)
        val base = if (args.keep) File(context.filesDir, "partages") else File(context.cacheDir, "envois")
        if (!args.keep) prune(base)
        val dir = File(base, UUID.randomUUID().toString())
        if (!dir.mkdirs()) throw IOException("dossier ${dir.path} non créé")
        val out = File(dir, safeName(displayName(uri)))
        val input = context.contentResolver.openInputStream(uri) ?: throw IOException("document illisible")
        input.use { i -> FileOutputStream(out).use { o -> i.copyTo(o, 1 shl 16) } }
        invoke.resolve(JSObject().apply { put("path", out.absolutePath) })
      } catch (e: Exception) {
        invoke.reject(e.message ?: e.toString())
      }
    }
  }

  /** A finished file copied into the document the user chose: its name. */
  @Command
  fun exportUri(invoke: Invoke) {
    val args = invoke.parseArgs(ExportArgs::class.java)
    io.execute {
      try {
        val uri = Uri.parse(args.uri)
        val output = context.contentResolver.openOutputStream(uri, "wt") ?: throw IOException("document non modifiable")
        output.use { o -> FileInputStream(File(args.path)).use { i -> i.copyTo(o, 1 shl 16) } }
        invoke.resolve(JSObject().apply { put("name", displayName(uri) ?: "sauvegarde") })
      } catch (e: Exception) {
        invoke.reject(e.message ?: e.toString())
      }
    }
  }

  @Command
  fun openFile(invoke: Invoke) {
    val args = invoke.parseArgs(PathArgs::class.java)
    try {
      val file = File(args.path)
      val uri = FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", file)
      val mime = MimeTypeMap.getSingleton().getMimeTypeFromExtension(file.extension.lowercase()) ?: "*/*"
      val view = Intent(Intent.ACTION_VIEW)
        .setDataAndType(uri, mime)
        .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
      context.startActivity(view)
      invoke.resolve()
    } catch (e: ActivityNotFoundException) {
      Toast.makeText(context, Lang.tr(context, "Aucune application ne peut ouvrir ce fichier.", "No app can open this file."), Toast.LENGTH_SHORT).show()
      invoke.reject("no app")
    } catch (e: Exception) {
      Toast.makeText(context, Lang.tr(context, "Ouverture du fichier impossible.", "The file could not be opened."), Toast.LENGTH_SHORT).show()
      invoke.reject(e.message ?: e.toString())
    }
  }

  @Command
  fun showDownloads(invoke: Invoke) {
    try {
      context.startActivity(Intent(DownloadManager.ACTION_VIEW_DOWNLOADS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
      invoke.resolve()
    } catch (e: Exception) {
      Toast.makeText(context, Lang.tr(context, "Fichiers enregistrés dans Téléchargements/P2PShare.", "Files are saved in Download/P2PShare."), Toast.LENGTH_LONG).show()
      invoke.reject(e.message ?: e.toString())
    }
  }

  /** The app's language, for the texts shown from here (see Lang). */
  @Command
  fun setLanguage(invoke: Invoke) {
    val args = invoke.parseArgs(LangArgs::class.java)
    Lang.set(context, args.lang)
    P2PShareService.createChannels(context)
    invoke.resolve()
  }

  private fun open(url: String): HttpURLConnection {
    val connection = URL(url).openConnection() as HttpURLConnection
    connection.connectTimeout = 15_000
    connection.readTimeout = 30_000
    connection.instanceFollowRedirects = true
    if (connection.responseCode !in 200..299) {
      throw IOException("HTTP ${connection.responseCode}")
    }
    return connection
  }

  /** A small text from the web (the releases' latest.json). */
  @Command
  fun fetchText(invoke: Invoke) {
    val args = invoke.parseArgs(UrlArgs::class.java)
    io.execute {
      try {
        val connection = open(args.url)
        val text = try {
          connection.inputStream.bufferedReader().use { it.readText() }
        } finally {
          connection.disconnect()
        }
        invoke.resolve(JSObject().apply { put("text", text) })
      } catch (e: Exception) {
        invoke.reject(e.message ?: e.toString())
      }
    }
  }

  /**
   * Download a new version's APK (its progress as events), then hand it to
   * the system's installer, which asks the user and checks that it is
   * signed like the app installed. Installing apps must be allowed first:
   * its settings open, and the command fails with "permission".
   */
  @Command
  fun installApk(invoke: Invoke) {
    val args = invoke.parseArgs(UrlArgs::class.java)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && !context.packageManager.canRequestPackageInstalls()) {
      try {
        context.startActivity(
          Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${context.packageName}"))
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        )
      } catch (_: Exception) {
      }
      invoke.reject(
        Lang.tr(
          context,
          "Autorisez P2P Share à installer des applications, puis réessayez.",
          "Allow P2P Share to install apps, then try again."
        ),
        "permission"
      )
      return
    }
    io.execute {
      try {
        val dir = File(context.cacheDir, "update")
        dir.deleteRecursively()
        if (!dir.mkdirs()) throw IOException("${dir.path} not created")
        val apk = File(dir, "P2PShare.apk")
        val connection = open(args.url)
        try {
          val total = connection.contentLengthLong
          var done = 0L
          var told = 0L
          connection.inputStream.use { input ->
            FileOutputStream(apk).use { output ->
              val buffer = ByteArray(1 shl 16)
              while (true) {
                val n = input.read(buffer)
                if (n < 0) break
                output.write(buffer, 0, n)
                done += n
                // About every 512 KB: enough for the progress bar.
                if (done - told >= (1 shl 19)) {
                  told = done
                  send(JSObject().apply {
                    put("updateProgress", JSObject().apply {
                      put("done", done)
                      put("total", if (total > 0) total else JSONObject.NULL)
                    })
                  })
                }
              }
            }
          }
        } finally {
          connection.disconnect()
        }
        val uri = FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", apk)
        context.startActivity(
          Intent(Intent.ACTION_VIEW)
            .setDataAndType(uri, "application/vnd.android.package-archive")
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
        )
        invoke.resolve()
      } catch (e: Exception) {
        invoke.reject(e.message ?: e.toString())
      }
    }
  }

  /** Start again in a new process (see RestartActivity), then end this one. */
  @Command
  fun restart(invoke: Invoke) {
    val restart = Intent(context, RestartActivity::class.java)
      .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
      .putExtra(RestartActivity.EXTRA_PID, Process.myPid())
    try {
      context.startActivity(restart)
    } catch (e: Exception) {
      Toast.makeText(context, Lang.tr(context, "Rouvrez P2P Share pour appliquer le réglage.", "Open P2P Share again to apply the setting."), Toast.LENGTH_LONG).show()
    }
    // Answered once asked: the Rust side then ends this process.
    invoke.resolve()
  }

  // ─── The history key ───────────────────────────────────────────────────

  private fun vaultKey(create: Boolean): SecretKey? {
    val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    (store.getKey(KEY_ALIAS, null) as? SecretKey)?.let { return it }
    if (!create) return null
    val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
    generator.init(
      KeyGenParameterSpec.Builder(KEY_ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
        .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
        .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
        .setKeySize(256)
        .build()
    )
    return generator.generateKey()
  }

  /** Base64 key in, base64 (IV + sealed key) out. */
  @Command
  fun keystoreWrap(invoke: Invoke) {
    val args = invoke.parseArgs(DataArgs::class.java)
    try {
      val key = vaultKey(true) ?: throw IllegalStateException("clé non créée")
      val cipher = Cipher.getInstance("AES/GCM/NoPadding")
      cipher.init(Cipher.ENCRYPT_MODE, key)
      val sealed = cipher.iv + cipher.doFinal(Base64.decode(args.data, Base64.NO_WRAP))
      invoke.resolve(JSObject().apply { put("data", Base64.encodeToString(sealed, Base64.NO_WRAP)) })
    } catch (e: Exception) {
      invoke.reject(e.message ?: e.toString())
    }
  }

  /** Rejected with code "lost" when it will never open (key gone, or not its key). */
  @Command
  fun keystoreUnwrap(invoke: Invoke) {
    val args = invoke.parseArgs(DataArgs::class.java)
    try {
      val key = vaultKey(false)
      if (key == null) {
        invoke.reject("clé absente du Keystore", "lost")
        return
      }
      val blob = try {
        Base64.decode(args.data, Base64.NO_WRAP)
      } catch (e: IllegalArgumentException) {
        invoke.reject("clé scellée illisible", "lost")
        return
      }
      if (blob.size < GCM_IV + GCM_TAG_BITS / 8) {
        invoke.reject("clé scellée trop courte", "lost")
        return
      }
      val cipher = Cipher.getInstance("AES/GCM/NoPadding")
      cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(GCM_TAG_BITS, blob, 0, GCM_IV))
      val raw = cipher.doFinal(blob, GCM_IV, blob.size - GCM_IV)
      invoke.resolve(JSObject().apply { put("data", Base64.encodeToString(raw, Base64.NO_WRAP)) })
    } catch (e: AEADBadTagException) {
      invoke.reject("scellée avec une autre clé", "lost")
    } catch (e: Exception) {
      invoke.reject(e.message ?: e.toString())
    }
  }
}
