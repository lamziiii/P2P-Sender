package com.p2pshare.app

import android.content.Context
import java.util.Locale

/**
 * The language of the texts shown from here (notifications, toasts): the
 * app's, which the Rust side sets (src-tauri/src/lang.rs) and is kept for
 * the service started before it. The system's until then: French, or
 * English for any other language.
 */
object Lang {
  private const val PREFS = "p2pshare-lang"
  private const val KEY = "language"

  @Volatile
  private var english: Boolean? = null

  fun english(context: Context): Boolean = english ?: run {
    val saved = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(KEY, null)
    (if (saved != null) saved == "en" else Locale.getDefault().language != "fr").also { english = it }
  }

  fun set(context: Context, lang: String) {
    english = lang == "en"
    context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putString(KEY, lang).apply()
  }

  fun tr(context: Context, fr: String, en: String): String = if (english(context)) en else fr
}
