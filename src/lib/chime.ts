// Short alert sound for camera proximity alerts, via Web Audio (no asset to ship).

let ctx: AudioContext | null = null;

function context(): AudioContext | null {
  try {
    ctx ??= new AudioContext();
    if (ctx.state === "suspended") void ctx.resume();
    return ctx;
  } catch {
    return null;
  }
}

/**
 * Create/resume the audio context from a user gesture (pressing the locate button), so a later
 * alert, which fires without a gesture, isn't blocked by autoplay rules.
 */
export function primeAudio(): void {
  context();
}

/** Two short descending tones. Silently does nothing if audio is unavailable. */
export function playChime(): void {
  const c = context();
  if (!c) return;
  const t0 = c.currentTime;
  [880, 660].forEach((freq, i) => {
    const start = t0 + i * 0.18;
    const osc = c.createOscillator();
    const gain = c.createGain();
    osc.type = "sine";
    osc.frequency.value = freq;
    gain.gain.setValueAtTime(0.0001, start);
    gain.gain.exponentialRampToValueAtTime(0.3, start + 0.02);
    gain.gain.exponentialRampToValueAtTime(0.0001, start + 0.16);
    osc.connect(gain).connect(c.destination);
    osc.start(start);
    osc.stop(start + 0.17);
  });
}
