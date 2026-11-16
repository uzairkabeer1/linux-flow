# Wispr Flow dictation protocol (from desktop 1.6.1483)

This spec was reverse-engineered by static reading of the Electron bundle
(`.webpack/main/index.js`, `.webpack/renderer/hub/index.js`,
`.webpack/renderer/dist/recorderWorklet.js`). No network traffic was captured.
Anything marked **UNVERIFIED** is inferred and has not been tested against the servers.
Byte offsets like `@3967192` refer to `main/index.js`, and `mod 99593` refers to webpack module ids.

---

## 0. TL;DR architecture

| Purpose | Transport | Host | Notes |
|---|---|---|---|
| **Primary dictation** | **gRPC bidi stream** `flow_api.v1.TranscriptionService/TranscribeStream` (protobuf, TLS :443) | `inference.wisprflow.com` (default; may be overridden by `https://inference-info.wisprflow.com/dictation_url.txt`, but only to an allowlisted host) | Streams 40 ms PCM packets while you talk. Partial results come back as `State`, and the final result as `Result`. |
| **Fallback dictation** (raced in parallel with gRPC, or used alone when gRPC is down) | HTTPS `POST /llm/dictation`, JSON `{"request": {...}}` (or msgpack when the `UseMsgPack` flag is on) | `https://api.wisprflow.ai` | One request per chunk of up to 28 s of audio. Audio is a base64 WAV. **This is the easiest path for a Python client.** |
| 2nd-level fallback ASR | `POST /llm/asr` (WAV) or `/llm/asr_audio_chunks` (opus) | `https://api.wisprflow.ai` | Raw transcript only, with no formatting. |
| Login | Browser "device code" flow at `https://wisprflow.ai/login?device_code=…`. The client polls `GET https://api.wisprflow.ai/device/poll` or reads an SSE stream from `/device/stream`. | | The desktop app never posts credentials itself. |
| Token refresh | WorkOS: `POST https://api.workos.com/user_management/authenticate` (public client, no secret). Legacy Supabase: `POST https://dodjkfqhwrzqjwkfnthl.supabase.co/auth/v1/token?grant_type=refresh_token` | | Your current session is **WorkOS** (`authProviderId` is a 6-char string, i.e. `"workos"`). |

`wss://api.wisprflow.ai/` and `wss://api-east.wisprflow.ai/` exist as constants (mod 47708). Nothing in the dictation path uses them (**UNVERIFIED** what they are for; probably sync or realtime voice actions). `https://api-east.wisprflow.ai` is the **staging** base (`B2`), not a regional mirror.

---

## 1. Dictation request

### 1.1 Which host/transport is actually used

The main flow (mod 64472, `@4686916`) goes like this:

```js
if (Ft.A.isConnected()) { const e = Dn(1/0); Ft.A.sendAudioPackets(e) }      // gRPC stream open -> stream every packet
else if (VoiceActionsRealtimeClient.isConnected()) ;
else { /* every basetenChunkSizeSeconds (=28 s) of audio */
   const e = Dn(); ...
   if (a.ZZ.useOpus) transcribeChunk(e)              // opus packets -> audio_packets
   else { const t = Ln(e); transcribeChunk(null, t) } // WAV file -> audio
}
```

- `Ft.A` is the gRPC client class (`class rn`, mod 9363). Its host comes from `getDesiredGrpcHost()`, which returns the dictation-URL module's value (mod 52712):

```js
const o="inference.wisprflow.com",
      a=new Set([o,"wispr-ai-prod--prod-grpc-server-flowgrpcserver.us-east.modal.direct"]);
... r.net.fetch("https://inference-info.wisprflow.com/dictation_url.txt",{method:"GET",signal:AbortSignal.timeout(8e3)})
... const t=e.trim().toLowerCase(); return a.has(t)?t:null   // only allowlisted hosts accepted
```

  So `dictation_url.txt` contains a bare **hostname** (no scheme or path), and it must be one of those two hosts. The gRPC port is not specified, so grpc-js uses its default of **443** with TLS.
- The HTTP fallback URL is `${AriaWebClient.getBaseWebUrl()}/llm/dictation`, which is `https://api.wisprflow.ai/llm/dictation` in production (`S$="https://api.wisprflow.ai"`).
- **Race:** if the gRPC heartbeat stops for more than 4 s (2 s after commit), or the server lags more than 2 s behind the audio, the client also sends an HTTP `/llm/dictation` with the entire recording as one WAV, provided it is under 30 s (`HTTP_FALLBACK_MAX_AUDIO_DURATION_MS=3e4`). The first usable answer wins.

### 1.2 HTTP `POST /llm/dictation`

Source: `sendFlowRequest` (mod 99593 `@3970293`):

```js
n ? (m=(0,o.encode)({request:e}), f="application/octet-stream")   // msgpack (flag UseMsgPack)
  : (m=JSON.stringify({request:e}), f="application/json");
const g={method:"POST",url:`${a.AriaWebClient.getBaseWebUrl()}${this.DICTATION_PATH}`,redirect:"error"};
A.setHeader("Authorization",e.access_token);   // raw token, NO "Bearer "
A.setHeader("Content-Type",f);
A.setHeader("Accept-Encoding","identity");
```

- The client gives up after a 15 s timeout (`d.bP = 15e3`).
- If the response is ≥400, the client treats it as failed and does not use the body for anything (it only logs it).
- The response body is JSON when the request was JSON, and msgpack when the request was msgpack.

#### Body: `{"request": FlowRequest}`

Built by `serializeFlowRequest(access_token, audioPackets, wavAudio, binary, isFallback)` (`@3967192`):

```js
h={access_token:e,
   audio: n&&!r ? (0,S.dS)(n) : n,                 // base64(WAV) for JSON; raw bytes for msgpack
   audio_packets: t ? (0,b.qN)(t,r) : null,
   pipeline:p,                                      // this.pipelineSteps(!0)
   context:{app:{name,bundle_id,type,url}, ax_context, ocr_context, variable_names, file_names,
            dictionary_context:[...].slice(0,200), user_first_name, user_last_name,
            textbox_contents:u, content_text},
   personalization_style_settings, language: prefs.user.selectedLanguages??[], app: appType,
   user:{uuid: prefs.user.uuid ?? "00000000-0000-0000-0000-000000000000"},
   metadata:{session_id: prefs.sessionId??"unknown", environment:"production",
             client_platform: prefs.platform, client_version: appVersion, transcript_entity_uuid: ZZ.uuid},
   prev_asr_text:o, prev_llm_text:a, prev_detected_language:c, last_nonempty_status:l,
   debug_mode: isUsingDebuggingBase10, test_mode:!1}
```

| Field | Type | What the official client sends | Required? |
|---|---|---|---|
| `access_token` | string | Same JWT as the `Authorization` header | yes (sent every time) |
| `audio` | string (base64) or null | base64 of a **complete WAV file**, 16 kHz / mono / s16le (see §2). Sent on the WAV path. | one of `audio` / `audio_packets` |
| `audio_packets` | object or null | Opus path only, see §2.3. `null` on the WAV path. | one of the two |
| `pipeline` | string[] | `["transcribe","align","format"]` (see below) | yes (**UNVERIFIED** whether the server defaults it) |
| `context.app.name` | string | Foreground app name; default `""` | send it |
| `context.app.bundle_id` | string | default `""` | send it |
| `context.app.type` | string | `"email"`, `"ai"`, `"terminal"` or `"other"` (default `"other"`) | send it |
| `context.app.url` | string | default `""` | send it |
| `context.ax_context` | string[] | Words parsed from the accessibility tree; default `[]` | optional |
| `context.ocr_context` | string[] | default `[]` | optional |
| `context.variable_names` / `file_names` | string[] | Only filled for VS Code; default `[]` | optional |
| `context.dictionary_context` | string[] | Personal and team dictionary words (starred first), at most 200 | optional |
| `context.user_first_name` / `user_last_name` | string | From your profile | optional |
| `context.textbox_contents` | object or null | `null` when unavailable. Otherwise `{contents, beforeText, selectedText, afterText, accessibilityIsFunctioning:true, couldNotGetTextBoxInfo:false, isEditable}` (camelCase, each part ≤500 chars) | optional, use `null` |
| `context.content_text` | string | Nearby screen text; default `""` | optional |
| `personalization_style_settings` | object | Default `{"personalizationAppType":"unset","personalizationStyle":"default"}`. AppType is one of `personal`, `work`, `email`, `other`, `unset`. Style is one of `formal`, `casual`, `veryCasual`, `excited`, `default`. | send the default |
| `language` | string[] | Your selected language codes, e.g. `["en"]`. Codes come from the map in §6.3. `[]` means auto-detect. | optional |
| `app` | string | Same value as `context.app.type` | send it |
| `user.uuid` | string (UUID) | Wispr user id. Equals the JWT claim `urn:wispr:user_external_id` (WorkOS) or `sub` (Supabase) | yes |
| `metadata.session_id` | string (UUID) | `config.json → prefs.sessionId` | yes |
| `metadata.environment` | string | `"production"` | yes |
| `metadata.client_platform` | string | `process.platform`: `"darwin"` or `"win32"`. **UNVERIFIED** whether `"linux"` is accepted. | yes |
| `metadata.client_version` | string | `"1.6.1483"` | yes |
| `metadata.transcript_entity_uuid` | string (UUID) | New random UUID for each dictation | yes |
| `prev_asr_text` | string | All earlier chunks' `asr_text` joined with spaces. `""` for the first or only chunk. | yes |
| `prev_llm_text` | string | Most recent non-empty `llm_text`, or `""` | yes |
| `prev_detected_language` | string or null | Most recent detected language, or `null` | yes |
| `last_nonempty_status` | string or null | `"raw_transcript"`, `"formatted"` or `null` | yes |
| `debug_mode` | bool | `false` | yes |
| `test_mode` | bool | `false` | yes |

#### `pipeline` (from `pipelineSteps(true)`)

```js
static pipelineSteps(e=!1){const t=e&&p.To.includes(y.ZZ.status), n=(0,E.D)(),
  r=[l.LT.TRANSCRIBE,l.LT.ALIGN], i=!(t||n===u.$Q.Raw);
  i&&r.push(l.LT.FORMAT);
  n===u.$Q.OnlyCapitalizeProperNouns&&r.push(l.LT.LOWERCASE_SENTENCES); return r}
// LT: transcribe | align | format | lowercase_sentences | slack_tagging | email_signature
```

- **Default for the final chunk:** `["transcribe","align","format"]`.
- Chunks sent while the user is still recording (status is initializing, listening or stopping) skip `"format"`.
- When formatting is set to "No AI formatting", the pipeline is `["transcribe","align"]`.
- With the "Casual messaging" setting, `"lowercase_sentences"` is appended.

#### Minimal context that should work (UNVERIFIED that every key is needed; it mirrors the client's defaults)

```json
{"request":{
  "access_token":"<JWT>",
  "audio":"<base64 WAV>",
  "audio_packets":null,
  "pipeline":["transcribe","align","format"],
  "context":{"app":{"name":"","bundle_id":"","type":"other","url":""},
             "ax_context":[],"ocr_context":[],"variable_names":[],"file_names":[],
             "dictionary_context":[],"user_first_name":"","user_last_name":"",
             "textbox_contents":null,"content_text":""},
  "personalization_style_settings":{"personalizationAppType":"unset","personalizationStyle":"default"},
  "language":["en"],
  "app":"other",
  "user":{"uuid":"<your wispr user uuid>"},
  "metadata":{"session_id":"<uuid>","environment":"production","client_platform":"win32",
              "client_version":"1.6.1483","transcript_entity_uuid":"<new uuid4>"},
  "prev_asr_text":"","prev_llm_text":"","prev_detected_language":null,"last_nonempty_status":null,
  "debug_mode":false,"test_mode":false}}
```

**Chunking:** the official client sends at most 28 s of audio per POST (`basetenChunkSizeSeconds = 28`). For longer recordings it sends consecutive POSTs and threads `prev_asr_text`, `prev_llm_text` and so on through them. The final text for a multi-chunk session is the last response's `pasted_text || llm_text`, falling back to the concatenated `asr_text` (`@4741488`). **UNVERIFIED** whether the server rejects requests longer than 28 s.

### 1.3 gRPC streaming (primary path)

- **Target:** `inference.wisprflow.com:443`, TLS. The client uses Node's root CAs plus self-signed system root CAs, so standard public roots should be enough.
- **Method:** `/flow_api.v1.TranscriptionService/TranscribeStream` (client and server streaming). There is also a unary `/Transcribe` that takes the same Request and returns the same Response.
- **Call metadata** (from `getRpcOptions`):

```js
{options:{meta:{authorization:`Bearer ${e}`,"disable-formatting":t?"false":"true"},timeout:this.STREAM_TIMEOUT},url:n}
```

  - `authorization`: `Bearer <JWT>`. Note this is **with** `Bearer`, unlike the HTTP path.
  - `disable-formatting`: `"false"` (or `"true"` for raw output).
  - Deadline: 23 min (`STREAM_DEADLINE_MS = 20 min + 3 min`).
- **Keepalive:** ping every 4 s (`grpc.keepalive_time_ms=4000`, `keepalive_permit_without_calls=1`).
- **Response headers** the client reads: `x-replica-id` and `x-proxy-region` (informational only).

#### Message sequence (from `processPendingRequest`, `startGrpcStream`, `handleResponse`)

```js
const s=e?this.makePayload(e):void 0, o=n?this.getContext():void 0,
      a=t?pt.TRUE:pt.FALSE, c=r?this.getInit():void 0;
const t={payload:s,context:o,commit:a,init:c}; await i.requests.send(t);
... if(t){ /* final */ this.endedStream=!0; ... await i.requests.complete(); }
```

1. **Open** the stream with the metadata above. There is no separate auth message.
2. **First Request:** `{init: Init, context: Context, payload: {audio_packets: {items: [...]}}, commit: COMMIT_FALSE}`.
   - `init` is sent exactly once, in the first message after the stream opens (`pendingInit=true`).
   - `context` is sent whenever the app or textbox context changes. Sending it once with the first message is fine.
3. **Audio Requests:** `{payload: {audio_packets: {items: [pkt, pkt, ...]}}, commit: COMMIT_FALSE}`.
   - The client batches whatever packets are pending; each item is one 40 ms WAV packet.
   - Each request may carry 1 to 18000 items (buf.validate).
4. **Final Request:** `{payload: <remaining packets or none>, commit: COMMIT_TRUE}`. Then **half-close** the send side (`requests.complete()`).
5. **Server → client Response messages** (oneof-like; any subset may be set):
   - `heartbeat {audio_received_seconds: float, commit_ack: bool}` arrives periodically. `commit_ack=true` acknowledges the commit. The client expects some response at least every 4 s before commit and every 2 s after.
   - `state` carries a partial result: `state.raw_text.content`, `state.formatted_text.content`, `state.progress` (transcriber/formatter durations, `*_finished` flags), and audio quality numbers.
   - `result` carries the final answer: `result.output.plaintext` (string), `result.output.html` (string, the pasted text, may contain HTML lists), `result.status` (`RESULT_STATUS_FORMATTED=1`, `ERROR=2` or `RAW_TRANSCRIPT=3`), `result.output.languages`, `result.response_time.*`, and `result.audio_duration`.
6. After a `result` arrives, the client closes the stream. If no result arrives within 20 min of the commit (backstop), it fails.

When building its own response record, the client prefers the last `state` texts over `output`:

```js
asr_text: this.requestState?.rawText?.content || t.plaintext,
llm_text: this.requestState?.formattedText?.content || t.html,
pasted_text: t.html
```

#### Values the official client puts in `Init`

```js
getMetadata(){ const e="00000000-0000-0000-0000-000000000000";
 return{userId:prefs.user.uuid??e, sessionId:prefs.sessionId??e, requestId:ZZ.uuid,
        audioEncoding: useOpus?Pe.OPUS:Pe.WAV, environment:PRODUCTION,
        client:{name:"Wispr Flow",platform:MACOS|WIN32|UNSPECIFIED,version:{major,minor,patch}},
        debug:false, referralCode:prefs.cache.referralCode||"", alwaysWaitScribe:!1}}
```

- `preferences.user`: `{first_name, last_name, email}`. **UNVERIFIED:** the email field has a buf.validate `email` rule, so either send your real email or omit the `user` sub-message entirely.
- `preferences.language`: enum list, mapped from the language codes in §6.3.
- `preferences.vocabulary`: dictionary words.
- `preferences.replacements`: maps of personal/team replacements and snippets.
- `preferences.style_config`: `{general_style:{work,email,personal,other: WRITING_STYLE_*}, override_style: UNSPECIFIED, tagging:{slack: bool}, signature:{flow_signature_prefix: UNSPECIFIED, sign_emails:false}, editing_strength: VERBATIM | LIGHT | MEDIUM}`.
- `init.state` is only set on retries.
- `metadata.user_id`, `session_id` and `request_id` **must be valid UUIDs** (buf.validate `string.uuid`).
- On Linux, `client.platform` would be `PLATFORM_UNSPECIFIED (0)`. **UNVERIFIED** whether the server rejects that.

#### Reconstructed `flow_api/v1/flow.proto`

Field numbers and types were copied from the protobuf-ts descriptors (`@~4090000`). Scalar codes: T:9 is string, 12 is bytes, 13 is uint32, 2 is float, 8 is bool, 3 is int64, 5 is int32. buf.validate options are omitted.

```proto
syntax = "proto3";
package flow_api.v1;
import "google/protobuf/duration.proto";
import "google/protobuf/timestamp.proto";

enum Platform { PLATFORM_UNSPECIFIED=0; PLATFORM_MACOS=1; PLATFORM_WIN32=2; PLATFORM_IOS=3; PLATFORM_ANDROID=4; PLATFORM_WEB=5; PLATFORM_API=6; }
enum AppType { APP_TYPE_UNSPECIFIED=0; APP_TYPE_OTHER=1; APP_TYPE_BROWSER=2; APP_TYPE_PERSONAL_MESSAGING=3; APP_TYPE_WORK_MESSAGING=4; APP_TYPE_EMAIL=5; APP_TYPE_CHATBOT=6; APP_TYPE_DEVELOPER=7; }
enum ChatRole { CHAT_ROLE_UNSPECIFIED=0; CHAT_ROLE_USER=1; CHAT_ROLE_HUMAN=2; CHAT_ROLE_ASSISTANT=3; }
enum AudioEncoding { AUDIO_ENCODING_UNSPECIFIED=0; AUDIO_ENCODING_WAV=1; AUDIO_ENCODING_OPUS=2; }
enum Environment { ENVIRONMENT_UNSPECIFIED=0; ENVIRONMENT_PRODUCTION=1; ENVIRONMENT_STAGING=2; ENVIRONMENT_DEVELOPMENT=3; ENVIRONMENT_TEST=4; }
enum WritingStyle { WRITING_STYLE_UNSPECIFIED=0; WRITING_STYLE_FORMAL=1; WRITING_STYLE_CASUAL=2; WRITING_STYLE_VERY_CASUAL=3; WRITING_STYLE_EXCITED=4; WRITING_STYLE_GENZ=5; }
enum EditingStrength { EDITING_STRENGTH_UNSPECIFIED=0; EDITING_STRENGTH_VERBATIM=1; EDITING_STRENGTH_LIGHT=2; EDITING_STRENGTH_MEDIUM=3; EDITING_STRENGTH_HEAVY=4; }
enum FlowSignaturePrefix { FLOW_SIGNATURE_PREFIX_UNSPECIFIED=0; FLOW_SIGNATURE_PREFIX_SPOKEN=1; FLOW_SIGNATURE_PREFIX_WRITTEN=2; }
enum Confidence { CONFIDENCE_UNSPECIFIED=0; CONFIDENCE_NONE=1; CONFIDENCE_LOW=2; CONFIDENCE_MEDIUM=3; CONFIDENCE_HIGH=4; }
enum Origin { ORIGIN_UNSPECIFIED=0; ORIGIN_MIXTURE=1; ORIGIN_WHISPER=2; ORIGIN_SCRIBE=3; ORIGIN_VOXTRAL=4; ORIGIN_QWEN=5; }
enum Commit { COMMIT_UNSPECIFIED=0; COMMIT_TRUE=1; COMMIT_FALSE=2; }
enum ResultStatus { RESULT_STATUS_UNSPECIFIED=0; RESULT_STATUS_FORMATTED=1; RESULT_STATUS_ERROR=2; RESULT_STATUS_RAW_TRANSCRIPT=3; }
enum SignatureStatus { SIGNATURE_STATUS_UNSPECIFIED=0; SIGNATURE_STATUS_ELIGIBLE=1; SIGNATURE_STATUS_ADDED=2; }
enum Language { LANGUAGE_UNSPECIFIED=0; LANGUAGE_ENGLISH_US=1; LANGUAGE_ENGLISH_UK=2; /* full list in §6.3 */ }

message Version { uint32 major=1; uint32 minor=2; uint32 patch=3; }
message Client { string name=1; Platform platform=2; Version version=3; }
message App { string name=1; string bundle_id=2; string url=3; AppType type=4; }
message Textbox { optional string contents=1; optional string before_text=2; optional string selected_text=3; optional string after_text=4; }
message DynamicVocabulary { repeated string screen_content_ax=1; repeated string screen_content_ocr=2; repeated string variable_names=3; repeated string file_names=4; }
message ChatMessage { ChatRole role=1; string content=2; string sender_id=3; }
message Conversation { string id=1; repeated string participants=2; repeated ChatMessage messages=3; }
message Context { App app=1; Textbox textbox=2; DynamicVocabulary vocabulary=3; string user_identifier=4; string content_text=5; string content_html=6; repeated App session_dictation_apps=7; bytes screenshot=8; Conversation conversation=9; }
message Metadata { string user_id=1; string session_id=2; string request_id=3; AudioEncoding audio_encoding=4; Environment environment=5; Client client=6; bool debug=7; string referral_code=8; bool always_wait_scribe=9; }
message GeneralWritingStyle { WritingStyle other=1; WritingStyle personal=2; WritingStyle work=3; WritingStyle email=4; }
message TaggingStyle { optional bool slack=1; }
message SignatureStyle { FlowSignaturePrefix flow_signature_prefix=1; optional bool flow_signature_hyperlink=2; optional bool sign_emails=3; }
message StyleConfig { GeneralWritingStyle general_style=1; WritingStyle override_style=2; TaggingStyle tagging=3; SignatureStyle signature=4; EditingStrength editing_strength=5; }
message AudioPackets { repeated bytes items=1; }          // 1..18000 items
message AudioFile { bytes data=1; }
message Payload { oneof audio { AudioPackets audio_packets=1; AudioFile audio_file=2; } }
message Text { string content=1; uint32 num_tokens=2; repeated Language languages=3; }
message Progress { google.protobuf.Duration transcriber=1; bool transcriber_finished=2; google.protobuf.Duration formatter=3; bool formatter_finished=4; bool post_processing_finished=5; }
message UserActions { bool check_mic=1; Language check_language=2; }
message WordAlignment { string word=1; float score=2; }
message CandidateBeam { Text text=1; float score=2; Confidence confidence=3; Origin origin=4; optional float alignment_score=5; optional string original_text=6; repeated WordAlignment word_alignments=7; }
message State { Progress progress=1; Text raw_text=2; Text formatted_text=3; Confidence confidence=4; UserActions user_actions=5; repeated CandidateBeam candidate_beams=6; optional float audio_snr=7; optional float audio_volume=8; optional float mean_alignment_score=9; optional uint32 formatted_raw_len=10; }
message User { string first_name=1; string last_name=2; string email=3; }
message StaticVocabulary { repeated string dictionary_personal=1; repeated string dictionary_team=2; repeated string dictionary_personal_starred=3; repeated string dictionary_team_starred=4; }
message Replacements { map<string,string> replacements_personal=1; map<string,string> replacements_team=2; map<string,string> snippets_personal=3; map<string,string> snippets_team=4; }
message Preferences { User user=1; repeated Language language=2; StaticVocabulary vocabulary=3; Replacements replacements=4; StyleConfig style_config=5; }
message Init { Metadata metadata=1; Preferences preferences=2; State state=3; }
message Request { Init init=1; Context context=2; Payload payload=3; Commit commit=4; google.protobuf.Timestamp proxy_received_timestamp=5; }
message ResponseTime { google.protobuf.Duration transcribe=1; google.protobuf.Duration transcribe_overhead=2; google.protobuf.Duration format=3; google.protobuf.Duration format_overhead=4; google.protobuf.Duration post_process=5; google.protobuf.Duration total=6; google.protobuf.Duration external_asr_time=7; google.protobuf.Duration external_llm_time=8; google.protobuf.Duration generate_queue_duration=9; google.protobuf.Duration generate_work_duration=10; }
message Transcription { string html=1; string plaintext=2; uint32 num_tokens=3; repeated Language languages=4; float confidence=5; bool language_detection_low_confidence=6; }
message TranscriptionStats { uint32 word_deletions=1; uint32 context_word_count=2; uint32 word_substitutions=3; }
message Result { Transcription output=1; google.protobuf.Duration audio_duration=2; ResponseTime response_time=3; SignatureStatus signature_status=4; ResultStatus status=5; TranscriptionStats stats=6; string transcript_origin=7; string server_address=8; string format_skip_reason=9; }
message Heartbeat { float audio_received_seconds=1; bool commit_ack=2; }
message Response { Result result=1; State state=2; repeated State all_states=3; Heartbeat heartbeat=4; }

service TranscriptionService {
  rpc Transcribe(Request) returns (Response);
  rpc TranscribeStream(stream Request) returns (stream Response);
}
```

`Language` and `languages` are declared `repeat:1` (packed) in protobuf-ts, which is the normal proto3 packed encoding.

---

## 2. Audio encoding

### 2.1 Recorder

- **AudioContext:** `{sampleRate: 16000, latencyHint: "interactive"}`. Constant `yj=16e3`; `RECORDING_SAMPLE_RATE = 16e3` in mod 59725.
- **Worklet:** `recorderWorklet.js` has `chunkSize=640`, i.e. **640 samples = 40 ms** per packet (`bj=40`).
- **Mono:** only `buffer[0]` (channel 0) is used.
- **Float to int16 conversion** (hub `Nj`):

```js
function Nj(e){const t=new Int16Array(e.length);for(let n=0;n<e.length;n++){const a=Math.max(-1,Math.min(1,e[n]));t[n]=a<0?32768*a:32767*a}return t}
```

- **Per-packet volume:** dBFS RMS of the float samples, `20*log10(sqrt(mean(x²))+1e-10)`.

### 2.2 WAV path (default; opus is behind feature flags `UseOpus` / `OpusDesktopExperiment`)

- **One packet** is 640 samples × 2 bytes = **1280 bytes of raw PCM s16le, 16 kHz, mono, with no header**. Sent as `{packet, volume, duration:40, encoding:"wav"}`.
- **gRPC:** each `AudioPackets.items[i]` is one of those 1280-byte raw PCM packets, with `Metadata.audio_encoding = AUDIO_ENCODING_WAV`. **UNVERIFIED** whether the server needs exactly 1280-byte items, but it is safest to send exactly that.
- **HTTP `audio`:** `Ln()`/`Yj()` concatenate the packets and wrap them in a canonical 44-byte WAV header. `dS()` then base64-encodes the result:

```js
function o(e,t,n=1){ ... l("RIFF"),u(36+r),l("WAVE"),l("fmt "),u(16),d(1),d(n),u(t),u(s),d(i),d(16),l("data"),u(r) ...}
// PCM format 1, channels 1, sampleRate 16000, byteRate 32000, blockAlign 2, bitsPerSample 16
const a=e=>Buffer.from(e).toString("base64")     // dS
```

  This is equivalent to Python's `wave` module writing 1 channel, sampwidth 2 and 16000 Hz, then `base64.b64encode`.

### 2.3 Opus path (`audio_packets`, `qN()`)

- **Encoder:** WebCodecs `AudioEncoder` with `{codec:"opus", sampleRate:16000, numberOfChannels:1, bitrate:64000}`.
- **Packets:** each encoder output chunk is one **raw Opus packet**, with no Ogg container and no length prefix. `duration:20` means 20 ms frames.
- **HTTP body field:**

```js
Pn=(e,t)=>{const n=t?e.packets:e.packets.map(e=>(0,vn.nN)(e)), r=t?"binary":"ascii85",
  s={packets:n, volumes:e.volumes, packet_duration:e.packetDuration/1e3, audio_encoding:e.encoding, byte_encoding:r}; ...}
```

  So `audio_packets = {packets: [str|bytes], volumes: [float dB], packet_duration: 0.02, audio_encoding: "opus", byte_encoding: "ascii85"|"binary"}`.
- **`ascii85` encoding:** npm `ascii85` defaults, i.e. the Adobe/btoa alphabet `!`..`u`, `z` for all-zero groups, **no `<~ ~>` delimiters**. This matches Python's `base64.a85encode(pkt)` with its defaults.
- **`binary`:** used only for msgpack bodies.
- **Recommendation:** stay on the WAV path. It is the default and the easiest to produce.

---

## 3. Response

### 3.1 HTTP `/llm/dictation` response (JSON)

Field list from the client's default/error template `YF` (mod 32508):

```js
{status, asr_time, asr_text, llm_time, llm_text, pasted_time, pasted_text, pipeline_time, pipeline_text,
 total_time, cursor_position, detected_language, average_log_prob, starts_with_proper_noun,
 component_times:{}, generated_tokens, formatting_divergence_score, fallback_english_llm_time,
 fallback_non_english_llm_time, used_http, called_external_asr, transcript_origin, check_mic,
 check_language, email_signature_eligible, email_signature_added, did_substitution,
 language_detection_low_confidence, word_deletions, word_substitutions, server_address, format_skip_reason}
```

- **`asr_text`**: raw ASR transcript.
- **`llm_text`**: formatted (LLM-cleaned) text.
- **`pasted_text`**: what the app actually pastes. It may contain HTML list markup such as `<ol><li>`.
- **Final text rule** (`@4741488`): `pasted_text || llm_text || prev_asr_text`. If `pasted_text` is absent, the client falls back to its own formatting of `asr_text`.
- **Times** (`*_time`) are in **seconds** (float). The client multiplies them by 1000.
- **`detected_language`** is a code like `"en"`, or `"xx"` for undetermined.

**`status`** values (enum `nf`, mod 89424). Only these are expected from the server:

| Value | Meaning |
|---|---|
| `"formatted"` | OK, `llm_text`/`pasted_text` present |
| `"raw_transcript"` | OK, only `asr_text` (formatting skipped or failed) |
| `"empty"` | OK, no speech |
| `"fallback"` | OK |
| `"error"` | "recoverable_error": the client switches to the `/llm/asr` fallback |

Anything else is "malformed". The full enum also contains client-side states: `verification_failed`, `no_audio`, `extension_other`, `extension_paste`, `dismissed`, `processing` and `timeout`.

```js
const O=new Set([p.nf.RawTranscript,p.nf.Formatted,p.nf.Empty,p.nf.Fallback]);
function C(e){return void 0!==e&&O.has(e)?"ok":e===p.nf.Error?"recoverable_error":"malformed"}
```

### 3.2 Error shape

- **`/llm/dictation`:** any HTTP status ≥400 counts as a failure. The body is only logged. **UNVERIFIED:** it is likely FastAPI style.
- **Other AriaWebClient endpoints:** the client extracts the error message from FastAPI-style bodies:

```js
ge=e=>{... const{detail:t}=e; if("string"==typeof t)return t; ... const{message:n,detail:r}=t; return n ?? r}
```

  So errors look like `{"detail": "..."}` or `{"detail": {"message"|"detail"|"code": ...}}`.
- **Enterprise IP allowlist block:** a 4xx with `{"code":"enterprise_ip_not_allowed","enterprise_id":"..."}`, possibly nested under `detail`. The app forces re-auth when it sees this.
- **Token expired:** presumably 401. **UNVERIFIED** status code; refresh and retry once.
- **gRPC errors:** standard grpc status codes. `RESULT_STATUS_ERROR` inside `result.status` maps to `"error"`.

### 3.3 Fallback `/llm/asr` (simple raw ASR)

```js
const i={audio:(0,ie.dS)(e),language:r,prompt:""};   // r = single selected language or prev detected, else ""
await Ce.request("post","/llm/asr",i,{timeoutOverride:12e3 ...})   // -> {content, detected_language}
```

The client reads `o.content` and `o.detected_language`, and treats the result as `status: raw_transcript`.

---

## 4. Request headers / AriaWebClient

`Ce.request(method, path, body, opts)` (`@3878389`):

```js
let a=`${Ce.getBaseWebUrl()}${t}`; r?.urlParams&&(a+=`?${new URLSearchParams(r.urlParams).toString()}`);
const u={"Content-Type":"application/json",Authorization:o,
         "Cache-Control":"no-cache, no-store, must-revalidate",Pragma:"no-cache",Expires:"0",
         ...(0,G.c1)()};   // c1() = Sentry trace headers (sentry-trace / baggage) - optional
```

- **URL:** `getBaseWebUrl()` is `https://api.wisprflow.ai` in production, `https://api-east.wisprflow.ai` when the staging flag is on, and `BASE_WEB_URL` or `http://127.0.0.1:8000` for local.
- **`Authorization`** is the **raw access-token JWT with no `Bearer` prefix**, for both `/llm/dictation` and the AriaWebClient REST calls. gRPC uses `Bearer <jwt>`.
- **No custom client headers:** there is no `x-client-version`, `x-platform`, device id or API key header. Version, platform and session id travel **inside the JSON body** (`metadata`).
- **User-Agent:**
  - Requests go through Electron `net.request` with the default session, so the UA is Electron's default Chromium UA (something like `Mozilla/5.0 … WisprFlow/1.6.1483 Chrome/… Electron/… Safari/537.36`). **UNVERIFIED:** I found no code that checks or sets the UA for API calls.
  - WorkOS refresh calls set `User-Agent: wispr-desktop-main/<version>`.
  - gRPC uses the grpc-js default UA.
- **Unauthenticated calls** (`requestWithoutAuth`) send only `Content-Type: application/json`. They are used for `/device/poll`, `/geo` and pre-login feedback.

---

## 5. Auth end to end

### 5.1 How the desktop app logs in

There are no passwords or OTP calls in the desktop app. It uses a browser-based **device-code** flow (mod 29425):

```js
const J=async()=>{const e=(0,r.randomUUID)(), ...
 const n=((e,t=!0)=>{const n=new URLSearchParams({device_code:e,auto_signin:t?"true":"false"});
   n.set("skip_default_trial","true"),n.set("grant_weekly_bonus","true"),
   DesktopWorkosCapableAuth.enabled&&n.set("workos_capable","true");
   return`${u.I8}/login?${n.toString()}`})(e,t);     // I8 = "https://wisprflow.ai"
 i.shell.openExternal(n) ... $(e) /* SSE */; setInterval(()=>H(e),1e4) /* poll every 10 s */;
 setTimeout(()=>V(e),3e5) /* 5 min hard timeout */ }
```

1. The client generates `device_code = uuid4()`. It is stored for 15 min (`deviceCodeAuth` in session.json).
2. The client opens the browser at `https://wisprflow.ai/login?device_code=<uuid>&auto_signin=true&skip_default_trial=true&grant_weekly_bonus=true&workos_capable=true`. The user signs in on the website by whatever method the site offers (Google, Apple, email/magic link, SSO).
3. The client waits for tokens in one of three ways:
   - **SSE:** `GET https://api.wisprflow.ai/device/stream?device_code=<uuid>` with `Accept: text/event-stream`. Lines look like `data: {json}`. A message with `status:"complete"` carries tokens; `status:"timeout"` means expired.
   - **Poll:** `GET https://api.wisprflow.ai/device/poll?device_code=<uuid>` (no auth, 8 s timeout). The response is the same JSON once complete.
   - **Deep link** (alternative): `wispr-flow://auth/transfer/success?device_code=…&access_token=…&refresh_token=…&provider=…&first_name=…`.
4. The completion payload (fields read by the client) is:

```json
{"status":"complete","access_token":"<jwt>","refresh_token":"<opaque>","provider":"workos|supabase",
 "first_name":"","last_name":"","onboarding_completed":true,"referral":null,"team_invite_code":null,
 "dub_click_id":null,"fbp":null,"fbc":null,"onboarding_context":{...},"derived_traits":{...}}
```

  **UNVERIFIED:** what a still-pending poll returns. Probably `{"status":"pending"}`, or a 4xx/404 until complete. Treat anything without `access_token` as "keep waiting".
5. **Provider selection:** the `provider` field wins if present. Otherwise the client decodes the JWT `iss`: if it contains `workos` or `authkit`, the token is WorkOS; otherwise it is Supabase.
6. After login the app calls `POST /api/v1/user/...registerDevice` (see §7). This is non-blocking.

### 5.2 Which token goes into `access_token`

`iD()` returns `getSessionCached().accessToken` for the **active provider**. For you that is the WorkOS access token (`session.json → workosSession.accessToken`).

WorkOS access JWT:
- Signed RS256. `iss` host is `api.workos.com`.
- Claims: `auth_time, client_id, email, exp, iat, iss, jti, sid, sub, urn:wispr:user_external_id`.
- **Lifetime is 259200 s (3 days)**, measured from `exp - iat`.
- `urn:wispr:user_external_id` is the Wispr user UUID, the one used as `user.uuid` / `metadata.user_id`.

Legacy Supabase access JWT:
- `iss` host is `dodjkfqhwrzqjwkfnthl.supabase.co`. Lifetime is **604800 s (7 days)**.
- Its `sub` is the same user UUID.

**UNVERIFIED:** whether `api.wisprflow.ai` and `inference.wisprflow.com` still accept the Supabase token for migrated users. Use the WorkOS one.

### 5.3 Refresh

**WorkOS** (`@3839475`) uses the WorkOS node SDK with a **public client**: no API key, no client secret.

```js
new pa({clientId:e,fetchFn:Ma,timeout:15e3,config:{headers:{"User-Agent":`wispr-desktop-main/${version}`}}})
  .userManagement.authenticateWithRefreshToken({clientId:o,refreshToken:r.refreshToken})
// SDK serializer when no key: {grant_type:"refresh_token",client_id,refresh_token,organization_id,ip_address,user_agent}
//   -> this.workos.post("/user_management/authenticate", body)   base = https://api.workos.com
```

- **When the client refreshes:**
  - when `expiresAt - 120 s <= now`, checked by a 60 s heartbeat;
  - on restore;
  - when the session is read.
- **`client_id`:** comes from the access token's `client_id` claim. It is also stored in `workosSession.clientId` and always starts with `client_`.
- **Response:** `{user:{…}, organization_id, access_token, refresh_token, authentication_method?, impersonator?}`. The SDK maps it to camelCase.
- **Refresh tokens rotate.** The new `refresh_token` replaces the old one. The old token is consumed, and **reusing it, or racing the desktop app for it, will fail**.
- **Terminal statuses** (most 4xx) force re-login. Transient errors back off for 30 s.

**Supabase** (legacy; gotrue-js 2.72.0, standard). This is used only if `authProviderId=="supabase"`:

```
POST https://dodjkfqhwrzqjwkfnthl.supabase.co/auth/v1/token?grant_type=refresh_token
apikey: <supabase anon key>
Authorization: Bearer <supabase anon key>
Content-Type: application/json
X-Client-Info: gotrue-js/2.72.0      (optional)
{"refresh_token":"<refresh>"}
-> {access_token, token_type:"bearer", expires_in, expires_at, refresh_token, user:{...}}
```

The anon key is a public constant shipped in the app: export `BN` of mod 47708, next to `VI = "https://dodjkfqhwrzqjwkfnthl.supabase.co"`. You can extract it with `grep -o 'eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9[^"]*' main/index.js | head -1`.

### 5.4 Exact HTTP calls a CLI can make

**(a) Start login.** The site, not the API, collects the email.

```
device_code = uuid4()
open_browser("https://wisprflow.ai/login?device_code=%s&auto_signin=true&skip_default_trial=true&grant_weekly_bonus=true&workos_capable=true" % device_code)
```

There is no API call to "start" the flow. The device code is client-generated.

**(b) Complete it.** Poll every 2 to 10 s, for up to 5 to 15 min:

```
GET https://api.wisprflow.ai/device/poll?device_code=<device_code>
Content-Type: application/json
-> 200 {"status":"complete","access_token":...,"refresh_token":...,"provider":"workos",...}
```

Or stream it:

```
GET https://api.wisprflow.ai/device/stream?device_code=<device_code>
Accept: text/event-stream
-> data: {"status":"complete",...}
```

**(c) Refresh (WorkOS):**

```
POST https://api.workos.com/user_management/authenticate
Content-Type: application/json
Accept: application/json, text/plain, */*
User-Agent: wispr-desktop-main/1.6.1483      (what the app sends; probably not checked)
{"grant_type":"refresh_token","client_id":"<client_id claim from access JWT>","refresh_token":"<refresh_token>"}
-> 200 {"user":{...},"access_token":"<new jwt>","refresh_token":"<new rotating token>", ...}
```

---

## 6. Local files (key structure only; no values copied)

### 6.1 `%APPDATA%/Wispr Flow/session.json`

```
session.json : object
├─ "authProviderId" : string                    // "workos" | "supabase"  (yours: 6 chars -> "workos")
├─ "workosSession" : object                     // ACTIVE credentials for WorkOS users
│   ├─ accessToken : string (JWT, RS256)        // <- goes in Authorization / access_token
│   ├─ refreshToken : string (opaque, ~25 chars, rotating)
│   ├─ userId : string (UUID)                   // == JWT urn:wispr:user_external_id == user.uuid
│   ├─ email : string
│   ├─ avatarUrl : string
│   ├─ expiresAt : int (unix seconds, = JWT exp)
│   ├─ clientId : string ("client_…", 33 chars) // WorkOS client_id for refresh
│   └─ migration : object { userId: UUID, sourceSessionId: UUID, baseUrl: string ("https://api.wisprflow.ai") }
├─ "sb-dodjkfqhwrzqjwkfnthl-auth-token" : string (JSON-encoded Supabase session)
│   └─ <json> { access_token: JWT, token_type: "bearer", expires_in: int, expires_at: int,
│               refresh_token: string, user: { id: UUID, aud, role, email, …, app_metadata{}, user_metadata{}, identities[] } }
└─ "deviceCodeAuth" : { deviceCode: UUID, expiresAt: ms }   // only while a login is pending (absent now)
```

### 6.2 `%APPDATA%/Wispr Flow/config.json` (relevant keys only)

- `prefs.sessionId`: string. Becomes `metadata.session_id`.
- `prefs.deviceId`: string. Becomes `device_id` in device registration.
- `prefs.anonymousId`: string (analytics).
- `prefs.platform`: string (`"win32"`). Becomes `metadata.client_platform`.
- `prefs.version`: string.
- `prefs.user.uuid`: string (UUID). Becomes `user.uuid`.
- `prefs.user.firstName` / `lastName` / `email`: string.
- `prefs.user.selectedLanguages`: string[] (codes, e.g. `"en"`). Becomes `language`.
- `prefs.user.useAxContext`: bool.
- `prefs.user.internal`: object. `prefs.user.enterprise`: null or object.
- `prefs.cache.*` holds the referral code, access flags and so on.

### 6.3 Language codes, and the proto `Language` value for each

```
af:AFRIKAANS am:AMHARIC ar:ARABIC as:ASSAMESE az:AZERBAIJANI ba:BASHKIR be:BELARUSIAN bg:BULGARIAN bn:BENGALI bo:TIBETAN br:BRETON bs:BOSNIAN ca:CATALAN cs:CZECH cy:WELSH da:DANISH de:GERMAN dech:GERMAN_SWISS el:GREEK en:ENGLISH_US enca:ENGLISH_CA engb:ENGLISH_UK es:SPANISH et:ESTONIAN eu:BASQUE fa:PERSIAN fi:FINNISH fo:FAROESE fr:FRENCH gl:GALICIAN gu:GUJARATI ha:HAUSA haw:HAWAIIAN he:HEBREW hi:HINDI hien:HINGLISH hr:CROATIAN ht:HAITIAN_CREOLE hu:HUNGARIAN hy:ARMENIAN id:INDONESIAN is:ICELANDIC it:ITALIAN ja:JAPANESE jv:JAVANESE ka:GEORGIAN kk:KAZAKH km:KHMER kn:KANNADA ko:KOREAN la:LATIN lb:LUXEMBOURGISH ln:LINGALA lo:LAO lt:LITHUANIAN lv:LATVIAN mg:MALAGASY mi:MAORI mk:MACEDONIAN ml:MALAYALAM mn:MONGOLIAN mr:MARATHI ms:MALAY mt:MALTESE my:MYANMAR ne:NEPALI nl:DUTCH nn:NYNORSK no:NORWEGIAN oc:OCCITAN pa:PUNJABI pl:POLISH ps:PASHTO pt:PORTUGUESE ro:ROMANIAN ru:RUSSIAN sa:SANSKRIT sd:SINDHI si:SINHALA sk:SLOVAK sl:SLOVENIAN sn:SHONA so:SOMALI sq:ALBANIAN sr:SERBIAN su:SUNDANESE sv:SWEDISH sw:SWAHILI ta:TAMIL te:TELUGU tg:TAJIK th:THAI tk:TURKMEN tl:TAGALOG tr:TURKISH tt:TATAR uk:UKRAINIAN ur:URDU uz:UZBEK vi:VIETNAMESE yi:YIDDISH yo:YORUBA yue:CANTONESE zh:CHINESE_TRADITIONAL zhcn:CHINESE_SIMPLIFIED
```

Proto `Language` numbers:

```
UNSPECIFIED=0 ENGLISH_US=1 ENGLISH_UK=2 CHINESE_SIMPLIFIED=3 CHINESE_TRADITIONAL=4 GERMAN=5 SPANISH=6 RUSSIAN=7 KOREAN=8 FRENCH=9 JAPANESE=10 PORTUGUESE=11 TURKISH=12 POLISH=13 CATALAN=14 DUTCH=15 ARABIC=16 SWEDISH=17 ITALIAN=18 INDONESIAN=19 HINDI=20 HINGLISH=21 FINNISH=22 VIETNAMESE=23 HEBREW=24 UKRAINIAN=25 GREEK=26 MALAY=27 CZECH=28 ROMANIAN=29 DANISH=30 HUNGARIAN=31 TAMIL=32 NORWEGIAN=33 THAI=34 URDU=35 CROATIAN=36 BULGARIAN=37 LITHUANIAN=38 LATIN=39 MAORI=40 MALAYALAM=41 WELSH=42 SLOVAK=43 TELUGU=44 PERSIAN=45 LATVIAN=46 BENGALI=47 SERBIAN=48 AZERBAIJANI=49 SLOVENIAN=50 KANNADA=51 ESTONIAN=52 MACEDONIAN=53 BRETON=54 BASQUE=55 ICELANDIC=56 ARMENIAN=57 NEPALI=58 MONGOLIAN=59 BOSNIAN=60 KAZAKH=61 ALBANIAN=62 SWAHILI=63 GALICIAN=64 MARATHI=65 PUNJABI=66 SINHALA=67 KHMER=68 SHONA=69 YORUBA=70 SOMALI=71 AFRIKAANS=72 OCCITAN=73 GEORGIAN=74 BELARUSIAN=75 TAJIK=76 SINDHI=77 GUJARATI=78 AMHARIC=79 YIDDISH=80 LAO=81 UZBEK=82 FAROESE=83 HAITIAN_CREOLE=84 PASHTO=85 TURKMEN=86 NYNORSK=87 MALTESE=88 SANSKRIT=89 LUXEMBOURGISH=90 MYANMAR=91 TIBETAN=92 TAGALOG=93 MALAGASY=94 ASSAMESE=95 TATAR=96 HAWAIIAN=97 LINGALA=98 HAUSA=99 BASHKIR=100 JAVANESE=101 SUNDANESE=102 CANTONESE=103 GERMAN_SWISS=104 ENGLISH_CA=105 UNDETERMINED=106
```

---

## 7. Anti-abuse / client checks

**What the bundle does *not* contain:**
- no request signing, HMAC or nonce;
- no app attestation;
- no client certificate;
- no API key header;
- no version header;
- no TLS pinning. The client logs the peer cert but validates it with standard `checkServerIdentity`.

**What *is* there (all server-side or soft):**
1. **Bearer JWT.** The server must accept the WorkOS or Supabase JWT. Account entitlement is enforced server-side: `GET /api/v1/access` returns `flags` including `dictation`, and the client uses it for UI gating. Expect 4xx if your plan or quota (e.g. the weekly word limit) disallows dictation.
2. **Body metadata.** `client_platform`, `client_version`, `session_id` and `user.uuid` are sent in the body.
   - **UNVERIFIED:** whether the server rejects unknown platforms (`"linux"`) or old or unknown versions. Nothing in the client handles a "please update" error for dictation.
   - The safest option is to mirror `client_version: "1.6.1483"` and `client_platform` as the official app sends it.
   - The gRPC side enforces UUID format on `user_id`, `session_id` and `request_id` (buf.validate), and an email format on `preferences.user.email`.
3. **Device registration.** After login the client calls `AriaWebClient.registerDevice({device_type: "<platform>_<arch>", device_os_version, device_os_name, device_manufacturer, device_model, device_id: prefs.deviceId, ...})` on `POST /api/v1/user…` (the path is truncated in my read; **UNVERIFIED** exact path). It is fire-and-forget and **not** a precondition for dictation in the client code. **UNVERIFIED** whether the server requires it.
4. **Enterprise IP allowlists** produce `enterprise_ip_not_allowed`. These only apply to enterprise orgs.
5. **Rotating refresh tokens.** This is the practical gotcha. If your Linux client and the Windows app share one refresh token, whichever refreshes first invalidates the other. **Do your own device-code login on Linux** so it gets its own session.

---

## 8. Minimal working client (Python pseudo-code)

```python
import base64, io, json, time, uuid, wave, webbrowser, requests

API   = "https://api.wisprflow.ai"
WEB   = "https://wisprflow.ai"
WORKOS= "https://api.workos.com"
VERSION = "1.6.1483"
STORE = "~/.config/wispr-linux/session.json"   # your own store, NOT the Windows one

def jwt_claims(tok):
    p = tok.split(".")[1]; p += "=" * (-len(p) % 4)
    return json.loads(base64.urlsafe_b64decode(p))

# ---------- 1. login (device code) ----------
def login():
    dc = str(uuid.uuid4())
    webbrowser.open(f"{WEB}/login?device_code={dc}&auto_signin=true"
                    f"&skip_default_trial=true&grant_weekly_bonus=true&workos_capable=true")
    deadline = time.time() + 300
    while time.time() < deadline:
        r = requests.get(f"{API}/device/poll", params={"device_code": dc},
                         headers={"Content-Type": "application/json"}, timeout=8)
        if r.ok:
            d = r.json() or {}
            if d.get("status") == "complete" and d.get("access_token") and d.get("refresh_token"):
                return save_session(d["access_token"], d["refresh_token"], d.get("provider"))
            if d.get("status") == "timeout": break
        time.sleep(3)
    raise RuntimeError("login timed out")

def save_session(at, rt, provider=None):
    c = jwt_claims(at)
    prov = provider or ("workos" if ("workos" in c.get("iss","") or "authkit" in c.get("iss","")) else "supabase")
    s = {"provider": prov, "access_token": at, "refresh_token": rt, "exp": c["exp"],
         "client_id": c.get("client_id"),
         "user_id": c.get("urn:wispr:user_external_id") or c["sub"],
         "session_id": str(uuid.uuid4())}            # stable per install; persist it
    write_json(STORE, s); return s

# ---------- 2. refresh ----------
def ensure_fresh(s):
    if s["exp"] - 120 > time.time(): return s
    if s["provider"] == "workos":
        r = requests.post(f"{WORKOS}/user_management/authenticate", timeout=15,
              headers={"Content-Type": "application/json", "Accept": "application/json",
                       "User-Agent": f"wispr-desktop-main/{VERSION}"},
              json={"grant_type": "refresh_token", "client_id": s["client_id"],
                    "refresh_token": s["refresh_token"]})
        r.raise_for_status(); d = r.json()
        new = save_session(d["access_token"], d.get("refresh_token") or s["refresh_token"], "workos")
    else:  # supabase legacy
        r = requests.post("https://dodjkfqhwrzqjwkfnthl.supabase.co/auth/v1/token",
              params={"grant_type": "refresh_token"}, timeout=15,
              headers={"apikey": ANON_KEY, "Authorization": f"Bearer {ANON_KEY}"},
              json={"refresh_token": s["refresh_token"]})
        r.raise_for_status(); d = r.json()
        new = save_session(d["access_token"], d["refresh_token"], "supabase")
    new["session_id"] = s["session_id"]; write_json(STORE, new); return new

# ---------- 3. dictate (HTTP path) ----------
def pcm_to_wav_b64(pcm16le_mono_16k: bytes) -> str:
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1); w.setsampwidth(2); w.setframerate(16000); w.writeframes(pcm16le_mono_16k)
    return base64.b64encode(buf.getvalue()).decode()

def dictate(s, pcm: bytes, langs=("en",), app_type="other", raw=False):
    s = ensure_fresh(s)
    CH = 28 * 16000 * 2                      # 28 s per request, like the official client
    prev_asr, prev_llm, prev_lang, last_status, last = [], "", None, None, None
    te_uuid = str(uuid.uuid4())
    for off in range(0, max(len(pcm), 1), CH):
        req = {
          "access_token": s["access_token"],
          "audio": pcm_to_wav_b64(pcm[off:off+CH]),
          "audio_packets": None,
          "pipeline": ["transcribe", "align"] + ([] if raw else ["format"]),
          "context": {"app": {"name": "", "bundle_id": "", "type": app_type, "url": ""},
                      "ax_context": [], "ocr_context": [], "variable_names": [], "file_names": [],
                      "dictionary_context": [], "user_first_name": "", "user_last_name": "",
                      "textbox_contents": None, "content_text": ""},
          "personalization_style_settings": {"personalizationAppType": "unset",
                                             "personalizationStyle": "default"},
          "language": list(langs), "app": app_type,
          "user": {"uuid": s["user_id"]},
          "metadata": {"session_id": s["session_id"], "environment": "production",
                       "client_platform": "linux",       # UNVERIFIED; try "win32" if rejected
                       "client_version": VERSION, "transcript_entity_uuid": te_uuid},
          "prev_asr_text": " ".join(prev_asr), "prev_llm_text": prev_llm,
          "prev_detected_language": prev_lang, "last_nonempty_status": last_status,
          "debug_mode": False, "test_mode": False}
        r = requests.post(f"{API}/llm/dictation", json={"request": req}, timeout=15,
                          allow_redirects=False,
                          headers={"Authorization": s["access_token"],      # raw JWT, no "Bearer"
                                   "Accept-Encoding": "identity"})
        if r.status_code == 401:                       # UNVERIFIED code: refresh once & retry
            s = ensure_fresh({**s, "exp": 0}); continue
        r.raise_for_status(); last = r.json()
        st = last.get("status")
        if st == "error":                              # fallback: plain ASR
            fb = requests.post(f"{API}/llm/asr", timeout=12,
                    headers={"Authorization": s["access_token"], "Content-Type": "application/json"},
                    json={"audio": req["audio"], "language": langs[0] if len(langs)==1 else "", "prompt": ""}).json()
            last = {"status": "raw_transcript", "asr_text": fb.get("content"),
                    "detected_language": fb.get("detected_language")}
        if last.get("asr_text"): prev_asr.append(last["asr_text"])
        if last.get("llm_text"): prev_llm = last["llm_text"]
        if last.get("detected_language") and last["detected_language"] != "xx":
            prev_lang = last["detected_language"]
        if last.get("status") in ("raw_transcript", "formatted"): last_status = last["status"]
    if not last: return ""
    # pasted_text may contain simple HTML (<ol><li>…) -> strip/convert for terminal use
    return last.get("pasted_text") or last.get("llm_text") or " ".join(prev_asr)

# ---------- 3b. dictate (gRPC streaming path, lower latency) ----------
# protoc the .proto in §1.3 -> flow_pb2 / flow_pb2_grpc
# ch   = grpc.secure_channel("inference.wisprflow.com:443", grpc.ssl_channel_credentials(),
#          options=[("grpc.keepalive_time_ms",4000),("grpc.keepalive_permit_without_calls",1)])
# stub = flow_pb2_grpc.TranscriptionServiceStub(ch)
# def reqs():
#     yield Request(init=Init(metadata=Metadata(user_id=uid, session_id=sid, request_id=str(uuid4()),
#               audio_encoding=AUDIO_ENCODING_WAV, environment=ENVIRONMENT_PRODUCTION,
#               client=Client(name="Wispr Flow", platform=PLATFORM_UNSPECIFIED,
#                             version=Version(major=1,minor=6,patch=1483))),
#               preferences=Preferences(language=[LANGUAGE_ENGLISH_US])),
#           context=Context(app=App(type=APP_TYPE_OTHER)),
#           payload=Payload(audio_packets=AudioPackets(items=[first_1280B_pkt])), commit=COMMIT_FALSE)
#     for batch in mic_batches():            # each item = 1280 bytes s16le 16 kHz mono (40 ms)
#         yield Request(payload=Payload(audio_packets=AudioPackets(items=batch)), commit=COMMIT_FALSE)
#     yield Request(commit=COMMIT_TRUE)      # then generator ends -> half-close
# for resp in stub.TranscribeStream(reqs(), metadata=[("authorization", f"Bearer {at}"),
#                                                      ("disable-formatting","false")], timeout=23*60):
#     if resp.HasField("state"):  partial = resp.state.formatted_text.content or resp.state.raw_text.content
#     if resp.HasField("result"): final = resp.result.output.plaintext; break
```

---

## 9. Open questions / UNVERIFIED summary

- Whether the server accepts `client_platform: "linux"` or `Platform.UNSPECIFIED`. The official app only ever sends `darwin`/`win32` (`MACOS`/`WIN32`).
- The exact `/device/poll` response while pending, and its HTTP code.
- The HTTP status for an expired token (assumed 401).
- Whether every listed body field is required. The server is FastAPI-like (`detail` errors), so missing fields probably cause a 422.
- The exact device-registration path, and whether the server requires registration before allowing dictation.
- Whether gRPC needs exactly 1280-byte (40 ms) WAV items, and whether the server tolerates `init` without `preferences`.
- What `wss://api.wisprflow.ai/` is used for (not dictation).
- The feature flags `UseMsgPack`, `UseOpus` and `GrpcHttpRace` are server-driven. Their defaults for your account are unknown, but the JSON + WAV path is always supported by the client code.
