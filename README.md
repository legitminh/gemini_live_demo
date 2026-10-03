# Booth

A Rust voice demo. You talk, **Gemini Live** decides the reply, and **Grok Voice** is the voice you hear.

Gemini's own audio is discarded. The live output transcript is streamed to Grok's text-to-speech websocket, and that PCM is what the browser plays.

```
microphone → Rust → Gemini Live (audio in, transcript out)
                 ↘ Grok Voice (transcript in, speech out) → speakers
```

## Run

```bash
cp .env.example .env
# fill in GEMINI_API_KEY and XAI_API_KEY
cargo run
```

Open http://127.0.0.1:8080

Run `cargo run` from the project directory so it finds `.env`. The page re-reads that file every time you click Start, and it prints the path it loaded. If Gemini or Grok rejects the session, the reason stays on the page.

| Variable | Purpose |
| --- | --- |
| `GEMINI_API_KEY` | Google AI Studio key. `GOOGLE_API_KEY` is also accepted. |
| `XAI_API_KEY` | xAI key used for Grok streaming TTS. `GROK_API_KEY` is also accepted. |
| `GEMINI_LIVE_MODEL` | Optional. Defaults to `gemini-3.8-live`. |
| `PORT` | Optional. Defaults to `8080`. |

Keys stay on the server. The page only learns whether each key is present.

## What the session does

1. The browser captures the mic as 16 kHz 16-bit PCM and sends it over `/ws`.
2. The server opens a Gemini Live websocket, asks for audio responses plus input and output transcripts, and forwards the mic audio.
3. Gemini audio chunks are dropped. Output transcript fragments are sent to `wss://api.x.ai/v1/tts` as `text.delta` / `text.done`.
4. Grok returns 24 kHz PCM. The browser plays it. Talking over Grok sends a barge-in, which clears Grok and lets Gemini interrupt.

You can also type a line. That is sent as a Gemini client turn and spoken by Grok the same way.

Headphones work better than speakers. Echo cancellation is enabled, but a loud room can still trip the barge-in.

## Tests

```bash
cargo test
```
