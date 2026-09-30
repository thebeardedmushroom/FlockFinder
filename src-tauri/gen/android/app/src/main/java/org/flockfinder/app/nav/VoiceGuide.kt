package org.flockfinder.app.nav

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.speech.tts.TextToSpeech
import android.speech.tts.UtteranceProgressListener
import androidx.media.AudioAttributesCompat
import androidx.media.AudioFocusRequestCompat
import androidx.media.AudioManagerCompat
import org.flockfinder.app.R
import java.util.Locale

/**
 * Spoken guidance: text-to-speech in US English, played as navigation audio (so it follows
 * the phone to Bluetooth, and other apps' audio ducks under it rather than stopping).
 *
 * - Each prompt takes transient, duckable audio focus and gives it back when done.
 * - At most one prompt waits while another is being spoken; a newer one replaces it, and one
 *   that waited too long is dropped, so nothing out of date is said.
 * - Nothing is said during a phone call.
 * - Camera alerts start with a chime (res/raw/camera_chime.wav).
 */
class VoiceGuide(private val context: Context, private val onUnavailable: () -> Unit) : TextToSpeech.OnInitListener {
  private val audio = context.getSystemService(Context.AUDIO_SERVICE) as AudioManager
  private val main = Handler(Looper.getMainLooper())
  private val tts = TextToSpeech(context, this)
  private var ready = false
  private var speaking = false
  private var pending: Triple<String, Boolean, Long>? = null
  private var focus: AudioFocusRequestCompat? = null
  private var onIdle: (() -> Unit)? = null
  private var counter = 0
  /** The utterance whose end frees the voice (a camera alert's chime comes before it). */
  private var lastId: String? = null

  private val attributes = AudioAttributes.Builder()
    .setUsage(AudioAttributes.USAGE_ASSISTANCE_NAVIGATION_GUIDANCE)
    .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
    .build()

  override fun onInit(status: Int) {
    if (status != TextToSpeech.SUCCESS) {
      onUnavailable()
      return
    }
    val lang = tts.setLanguage(Locale.US)
    if (lang == TextToSpeech.LANG_MISSING_DATA || lang == TextToSpeech.LANG_NOT_SUPPORTED) {
      onUnavailable()
      return
    }
    tts.setAudioAttributes(attributes)
    tts.addEarcon(CHIME, context.packageName, R.raw.camera_chime)
    tts.setOnUtteranceProgressListener(object : UtteranceProgressListener() {
      override fun onStart(utteranceId: String?) {}

      override fun onDone(utteranceId: String?) {
        main.post { finished(utteranceId) }
      }

      @Deprecated("Deprecated in Java")
      override fun onError(utteranceId: String?) {
        main.post { finished(utteranceId) }
      }

      override fun onError(utteranceId: String?, errorCode: Int) {
        main.post { finished(utteranceId) }
      }
    })
    ready = true
  }

  private fun inCall(): Boolean =
    audio.mode == AudioManager.MODE_IN_CALL || audio.mode == AudioManager.MODE_IN_COMMUNICATION || audio.mode == AudioManager.MODE_RINGTONE

  /** Say `text` now, or after what is being said (replacing anything already waiting). */
  fun speak(text: String, camera: Boolean) = main.post {
    if (!ready || inCall()) return@post
    if (speaking) {
      pending = Triple(text, camera, SystemClock.elapsedRealtime())
    } else {
      say(text, camera)
    }
  }

  private fun say(text: String, camera: Boolean) {
    val request = AudioFocusRequestCompat.Builder(AudioManagerCompat.AUDIOFOCUS_GAIN_TRANSIENT_MAY_DUCK)
      .setAudioAttributes(
        AudioAttributesCompat.Builder()
          .setUsage(AudioAttributesCompat.USAGE_ASSISTANCE_NAVIGATION_GUIDANCE)
          .setContentType(AudioAttributesCompat.CONTENT_TYPE_SPEECH)
          .build()
      )
      .setOnAudioFocusChangeListener { }
      .build()
    if (AudioManagerCompat.requestAudioFocus(audio, request) != AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
      // A call or another app holding exclusive focus: skip this prompt.
      return
    }
    focus = request
    speaking = true
    val id = "p${++counter}"
    val params = Bundle()
    if (camera) {
      tts.playEarcon(CHIME, TextToSpeech.QUEUE_FLUSH, params, "chime$counter")
      tts.speak(text, TextToSpeech.QUEUE_ADD, params, id)
    } else {
      tts.speak(text, TextToSpeech.QUEUE_FLUSH, params, id)
    }
    lastId = id
  }

  private fun finished(utteranceId: String?) {
    if (utteranceId != lastId) return
    speaking = false
    val next = pending
    pending = null
    if (next != null && SystemClock.elapsedRealtime() - next.third < STALE_MS && !inCall()) {
      say(next.first, next.second)
      return
    }
    focus?.let { AudioManagerCompat.abandonAudioFocusRequest(audio, it) }
    focus = null
    onIdle?.let { it(); onIdle = null }
  }

  /** Release the engine, after anything being said when `afterSpeech` (at most a few seconds). */
  fun shutdown(afterSpeech: Boolean, done: () -> Unit) = main.post {
    val release = {
      pending = null
      tts.stop()
      tts.shutdown()
      focus?.let { AudioManagerCompat.abandonAudioFocusRequest(audio, it) }
      focus = null
      done()
    }
    if (afterSpeech && speaking) {
      var released = false
      val once = { if (!released) { released = true; release() } }
      onIdle = once
      main.postDelayed(once, 8_000)
    } else {
      release()
    }
  }

  companion object {
    private const val CHIME = "[camera]"
    /** A prompt that waited longer than this is out of date. */
    private const val STALE_MS = 4_000L
  }
}
