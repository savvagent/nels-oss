// Browser-native speech-to-text dictation for the chat input, wrapping the Web
// Speech API (SpeechRecognition / webkitSpeechRecognition). Recognition runs
// entirely in the browser — no network or backend involvement.
//
// Unsupported browsers (notably Firefox) expose `supported === false` so callers
// can hide the mic affordance entirely rather than showing a button that fails.

const SpeechRecognition =
  typeof window !== "undefined"
    ? window.SpeechRecognition || window.webkitSpeechRecognition
    : undefined;

// Map Nels' base locale codes (see i18n) to the BCP-47 tags the recognizer
// expects. Falls back to en-US for anything unmapped.
const SPEECH_LANG = {
  en: "en-US",
  es: "es-ES",
  fr: "fr-FR",
  de: "de-DE",
  it: "it-IT",
  pt: "pt-PT",
};

// Create a dictation controller. State is rune-backed so consuming components
// reactively reflect listening/error changes.
export function createDictation() {
  let listening = $state(false);
  let error = $state(null); // null | "denied" | "error"
  let recognition = null;

  const supported = !!SpeechRecognition;

  // Begin a single dictation utterance for `lang` (a base locale code). Interim
  // results stream to `onResult(text)` so the input updates live as the user
  // speaks; recognition stops on its own at the end of the utterance.
  function start(lang, { onResult } = {}) {
    if (!supported || listening) return;
    error = null;
    recognition = new SpeechRecognition();
    recognition.lang = SPEECH_LANG[lang] || "en-US";
    recognition.interimResults = true;
    recognition.continuous = false;

    recognition.onresult = (event) => {
      let text = "";
      for (let i = 0; i < event.results.length; i++) {
        text += event.results[i][0].transcript;
      }
      onResult?.(text);
    };

    recognition.onerror = (event) => {
      // "no-speech" / "aborted" are benign (user said nothing or stopped) — stay
      // silent. Surface only permission denial and genuine failures.
      if (event.error === "not-allowed" || event.error === "service-not-allowed") {
        error = "denied";
      } else if (event.error !== "no-speech" && event.error !== "aborted") {
        error = "error";
      }
    };

    recognition.onend = () => {
      listening = false;
      recognition = null;
    };

    try {
      recognition.start();
      listening = true;
    } catch {
      // start() throws if invoked while already running; treat as non-fatal.
      listening = false;
      recognition = null;
    }
  }

  function stop() {
    recognition?.stop();
  }

  return {
    get supported() {
      return supported;
    },
    get listening() {
      return listening;
    },
    get error() {
      return error;
    },
    start,
    stop,
  };
}
