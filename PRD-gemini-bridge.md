# PRD — Gemini Bridge

**Nama kerja:** `gemini-bridge`
**Versi dokumen:** 1.0 (Draft untuk direview)
**Tanggal:** 30 September 2026
**Status:** Draft
**Pemilik produk:** Afi

---

## 1. Executive Summary

### 1.1 Problem Statement

Fitur-fitur Gemini (terutama **image generation**) hanya tersedia secara praktis bagi pengguna akhir melalui **Gemini Web** — mereka harus membuka browser, login, membuka menu generate image, menulis prompt, melampirkan referensi, lalu mengunduh hasilnya secara manual. Sementara itu jalur resmi (**Gemini API**) berbayar, dan justru **image generation adalah salah satu fitur yang paling mahal/tidak tersedia bebas** di API. Akibatnya aplikasi client yang ingin menawarkan fitur "bikin gambar dari prompt" tidak punya jalur murah yang layak.

### 1.2 Proposed Solution

**gemini-bridge** — sebuah **server self-hosted berbahasa Rust** yang menjembatani aplikasi client ke **Gemini Web** (gemini.google.com) melalui **API REST yang kompatibel dengan OpenAI**. Client cukup mengarahkan `base_url`-nya ke bridge, dan fitur Gemini Web — chat, streaming, image generation, upload referensi, percakapan persisten, tool calling, code execution surfacing — tersedia tanpa perlu membuka browser. Arsitektur inti mengadopsi filosofi **"everything is a plugin"** (diilhami deepseek-harness/Cordis) sehingga setiap kapabilitas bersifat *swappable, adaptive,* dan *reversible*.

### 1.3 Success Criteria (KPI terukur)

| # | Metrik | Target |
|---|---|---|
| 1 | **Overhead latency** yang ditambahkan bridge (bukan dari upstream Gemini) | **p50 ≤ 15 ms**, p95 ≤ 40 ms pada endpoint streaming |
| 2 | **Footprint memori** saat idle (tanpa request aktif) | **≤ 25 MB RSS**; saat streaming aktif ≤ 120 MB per 10 koneksi |
| 3 | **Cold start** (proses start → siap menerima request) | ≤ 150 ms |
| 4 | **Keberhasilan streaming chat** (end-to-end, tanpa intervensi) selama 7 hari | ≥ 99% request sukses, tanpa restart manual |
| 5 | **Cakupan parity OpenAI API** untuk endpoint inti | `POST /v1/chat/completions` (stream+nostream), `POST /v1/images/generations`, `POST /v1/files` = 100% lulus acceptance test |
| 6 | **Plugin reload tanpa downtime** | Memuat ulang plugin adapter (mis. setelah rotasi cookie) **tanpa menjatuhkan sesi aktif**, terverifikasi < 2 detik |
| 7 | **Efisiensi binary** | Single static binary **≤ 25 MB** (tanpa asset opsional), tanpa runtime eksternal |

---

## 2. User Experience & Functionality

### 2.1 User Personas

**P0 — Developer aplikasi client (Afi & tim).**
Membangun aplikasi (web/mobile/backend) yang butuh fitur AI. Tidak ingin mengurus browser automation, cookie rotation, atau protokol batchexecute. Ingin `base_url` + `api_key` seperti OpenAI, lalu jalan.

**P1 — Power user self-host.**
Menjalankan bridge di VPS/laptop sendiri, memakai satu akun Google. Butuh kontrol operasional: status sesi, rotasi kredensial, log, health check.

**P2 — Aplikasi non-cloud / offline-first.**
Aplikasi desktop yang ingin fitur AI gratis tanpa biaya API, dengan bridge berjalan lokal berdampingan.

### 2.2 User Stories & Acceptance Criteria

#### US-1 — Chat completion kompatibel OpenAI
> Sebagai developer client, saya ingin mengirim request chat ke bridge dengan format yang sama seperti OpenAI, sehingga saya tidak perlu menulis kode integrasi khusus.

**AC:**
- `POST /v1/chat/completions` menerima `model`, `messages[]` (role: system/user/assistant), `stream`, `temperature`, `max_tokens`.
- Respons non-stream identik bentuknya dengan OpenAI: `choices[0].message.content`, `finish_reason`, `usage`.
- Respons stream berformat **SSE** dengan `data: {...}` dan terminator `data: [DONE]`.
- Dukung penamaan model Gemini Web: `gemini-web-flash`, `gemini-web-pro`, `gemini-web-thinking`, `gemini-web-auto`, plus sufiks `@think=0..4`.
- Error upstream dipetakan ke kode HTTP yang benar (429, 503, 405→retry internal, 401).

#### US-2 — Image generation end-to-end
> Sebagai developer client, saya ingin memanggil `POST /v1/images/generations` dan menerima gambar jadi, sehingga aplikasi saya bisa menawarkan fitur generate image tanpa biaya API.

**AC:**
- `POST /v1/images/generations` menerima `prompt`, `n`, `size`, `response_format` (`url`/`b64_json`), dan opsional `reference_images[]` (URL atau base64).
- Bridge mengunggah referensi via resumable push upload, mengirim prompt ke StreamGenerate, dan **mengekstraksi URL gambar** dari payload respons (slot respons tree yang saat ini tidak ditangani gemini-web2api).
- Hasil dikembalikan sebagai URL yang di-proxy bridge (agar tidak expire) **atau** `b64_json`.
- Gambar tersimpan di cache lokal dengan metadata (prompt, model, waktu, seed bila ada).
- Endpoint `GET /v1/images/{id}` untuk mengambil ulang hasil.
- **Acceptance test nyata:** minimal 5 prompt berbeda + 2 prompt dengan referensi gambar, seluruhnya mengembalikan gambar valid (bukan teks kosong).

#### US-3 — Upload file / referensi multimodal
> Sebagai developer client, saya ingin melampirkan gambar sebagai referensi, sehingga hasil generate mengikuti gaya/referensi yang saya berikan.

**AC:**
- `POST /v1/files` menerima multipart upload, mengembalikan `id` file.
- `id` dapat direferensikan di `messages[].content[]` (format `image_url`) dan di `images/generations.reference_images`.
- Batas ukuran & tipe divalidasi (mime sniffing, tolak selain image/* untuk MVP), ada guard SSRF untuk URL referensi.
- File di-cache lokal; referensi ke Gemini menggunakan fileRef hasil push upload (di-cache agar tidak upload dua kali untuk file yang sama, berbasis hash konten).

#### US-4 — Percakapan persisten & branching
> Sebagai developer client, saya ingin percakapan bisa dilanjutkan dan punya cabang, sehingga aplikasi saya bisa punya fitur riwayat chat dan "regenerate"/"edit & kirim ulang".

**AC:**
- Bridge menyimpan `conversationId` + `responseId` + `candidateId` Gemini, dan memetakannya ke `conversation_id` internal.
- Request dengan `conversation_id` melanjutkan percakapan di Gemini (bukan sekadar menempel riwayat di prompt).
- `POST /v1/conversations/{id}/branch` membuat cabang dari message tertentu.
- `GET /v1/conversations` & `GET /v1/conversations/{id}/messages` menyajikan riwayat lokal, konsisten dengan state upstream.
- Fallback: bila ID upstream ditolak, bridge otomatis degradasi ke mode "replay history" dan menandainya di respons (`x-gemini-bridge-continuity: degraded`).

#### US-5 — Tool calling (emulasi)
> Sebagai developer client, saya ingin function calling seperti OpenAI, sehingga agent saya bisa memakai tool.

**AC:**
- `tools[]` + `tool_choice` diterima; bridge menyuntikkan instruksi skema dan mem-parsing output model menjadi `tool_calls[]` terstruktur.
- Hasil tool dikirim balik sebagai role `tool` dan diteruskan ke model.
- Ada validator yang menolak `tool_calls` malformed (fallback ke teks + warning terstruktur).
- Metrik: tingkat parse sukses ≥ 95% pada test suite 50 kasus.

#### US-6 — Gallery & manajemen media
> Sebagai power user, saya ingin melihat semua gambar yang pernah dihasilkan, sehingga saya bisa mengelola/mengambil ulang.

**AC:**
- UI web minimal (atau endpoint JSON) untuk listing gambar tersimpan: filter by tanggal/model/prompt, hapus, unduh.
- Endpoint `GET /gallery` (JSON) + `GET /gallery?format=html` untuk tampilan ringan.
- Tidak wajib SPA; boleh satu halaman HTML statis.

#### US-7 — Code execution & grounding surfacing
> Sebagai developer, saya ingin hasil code execution dan sitasi web search tidak dibuang, sehingga aplikasi saya bisa menampilkannya.

**AC:**
- Blok code execution (`code_stdout`) diekstrak ke field terstruktur `gemini_metadata.code_execution[]`, tidak dibuang.
- Sitasi/grounding dikembalikan di `gemini_metadata.citations[]`.
- Field tambahan bersifat opsional (tidak merusak kompatibilitas OpenAI).

#### US-8 — Operasional & kesehatan sesi
> Sebagai power user, saya ingin tahu kondisi sesi Gemini saya dan dipandu saat perlu intervensi.

**AC:**
- `GET /healthz` → status proses, uptime, versi.
- `GET /readyz` → status sesi Gemini (valid/stale/perlu re-auth), status build label (`bl`), umur cookie `1PSIDTS`.
- Rotasi `__Secure-1PSIDTS` otomatis; bila gagal → status `needs_reauth` + endpoint `POST /admin/reauth` (mode terpandu, mis. ambil cookie dari profil browser Camofox/browser yang ada).
- Log terstruktur JSON dengan request-id.
- Plugin reload (adapter) **tanpa restart proses** dan tanpa memutus stream aktif.

#### US-9 — Video & kapabilitas eksperimental
> Sebagai developer, saya ingin mencoba kapabilitas video bila tersedia.

**AC:**
- Adapter `video` didaftarkan sebagai plugin **eksperimental**, off-by-default.
- Bila upstream tidak menyediakan → mengembalikan `501 Not Implemented` dengan pesan jelas, bukan error 500.
- Bila tersedia → hasil dikembalikan sebagai URL/b64 dengan skema yang sama seperti image.

#### US-10 — Konfigurasi & deployment
> Sebagai power user, saya ingin menjalankan bridge dengan satu binary dan satu file config.

**AC:**
- Konfigurasi via file TOML (`bridge.toml`) + override environment.
- Menjalankan: `gemini-bridge --config bridge.toml` (single static binary, tanpa dependency runtime).
- `install` / `service` helper untuk systemd opsional (unit file di-generate).
- Docker image opsional (distroless/scratch, ≤ 30 MB).

### 2.3 Non-Goals (v1)

- ❌ **Bukan multi-tenant SaaS.** Tidak ada pendaftaran user, billing, kuota per user, atau isolasi kredensial antar-user.
- ❌ **Bukan proxy umum semua produk Google** (YouTube, Drive, Gmail). Fokus Gemini Web.
- ❌ **Tidak menjamin 100% parity OpenAI** untuk endpoint lanjutan (embeddings, fine-tuning, batch, assistants, realtime API).
- ❌ **Tidak menyediakan UI chat lengkap** (itu tugas aplikasi client). Bridge hanya menyediakan UI minimal untuk gallery & status.
- ❌ **Tidak mengaburkan risiko.** Bridge tidak menyembunyikan fakta bahwa ini automasi tidak resmi; dokumentasi wajib menyatakan risiko ToS.
- ❌ **Tidak ada model lain** (Claude/GPT/LLaMA) di v1 — tapi arsitektur plugin harus memungkinkan penambahannya tanpa refactor.

---

## 3. AI System Requirements

### 3.1 Komponen & Kapabilitas Upstream

| Kapabilitas | Endpoint upstream | Catatan |
|---|---|---|
| Chat / generate | `POST /_/BardChatUi/data/assistant.lamda.BardFrontendService/StreamGenerate` | `bl`, `hl`, `_reqid`, `rt=c`; body `f.req` |
| RPC tambahan | `POST /_/BardChatUi/data/batchexecute` | 18 rpcid teramati (list percakapan, feedback, image history) |
| Upload referensi | `POST https://content-push.googleapis.com/upload/` | Resumable push, 2 langkah, hasil = fileRef |
| Bootstrap sesi | `GET /app` | Ambil `bl`, `SNlM0e`, `thykhd`, `qKIAYe`, `Ylro7b`, `f.sid`; refresh cookie |
| Konten hasil | `https://googleusercontent.com/...` | URL gambar/attachment (perlu di-cache ke lokal karena bisa expire) |
| Auth | Cookie `__Secure-1PSID` + `1PSIDTS` + `SAPISID` | Header `Authorization: SAPISIDHASH <ts>_<sha1>` |

### 3.2 Strategi Evaluasi Kualitas

**E1 — Golden test suite (offline-ish, live upstream):**
- 50 prompt chat terstruktur (fakta, reasoning, panjang, kode, bahasa Indonesia/Inggris) → verifikasi non-empty, tidak terpotong, format benar.
- 20 prompt image (5 dengan referensi) → verifikasi gambar valid (magic bytes, dimensi > 0, lolos decode).
- 10 kasus tool calling → verifikasi JSON `tool_calls` valid.

**E2 — Resilience drills (chaos):**
- Simulasi `405` (BL basi) → bridge harus auto-refresh dan retry sukses tanpa error ke client.
- Simulasi `429` → backoff + antrian, client menerima 429 jujur bila batas tercapai.
- Simulasi cookie kedaluwarsa → status `needs_reauth`, bukan crash/loop.

**E3 — Performa (criterion #1 & #2):**
- `wrk`/`oha` 100 concurrent, 30 detik, endpoint chat non-stream dan streaming.
- Ukur overhead sebagai selisih waktu bridge vs. waktu upstream murni (diukur dari log internal dengan `t_upstream_done - t_upstream_start`).

**E4 — Efisiensi token/perilaku agent (opsional lanjut):**
- Bandingkan jumlah round-trip & token yang diperlukan agent untuk menyelesaikan 10 skenario umum lewat bridge vs. alternatif lain.

**Ambang lulus:** E1 ≥ 95% lulus, E2 100% skenario tertangani sesuai spesifikasi, E3 memenuhi KPI #1/#2.

---

## 4. Technical Specifications

### 4.1 Prinsip Arsitektur (filosofi project)

1. **Everything is a plugin.** Tidak ada "core istimewa". Adapter provider, penyimpanan media, rotasi kredensial, endpoint HTTP, bahkan loop pemrosesan request adalah plugin pada satu *context* bersama.
2. **Service-key DI, bukan urutan boot manual.** Plugin menyatakan dependensi (`inject: [identity, transport, store]`); urutan pemuatan = hasil resolusi kebutuhan.
3. **Reversible effects.** Setiap registrasi (route, tool, listener) menyertakan disposer — plugin dapat dilepas/dipasang ulang saat runtime (kritis untuk re-auth tanpa downtime).
4. **Typed event bus dengan dispatch mode eksplisit.** `emit` (observasi), `waterfall` (middleware: rate limit, redaksi, retry), `serial`, `parallel`, `bail`. Mode adalah bagian kontrak publik event.
5. **Request object immutable setelah dibangun.** Policy (rate limit, redaksi, transformasi) berjalan sebagai waterfall middleware; tidak ada plugin yang menulis ulang payload di tengah jalan.
6. **Adapter/retry separation.** Kosakata stream internal bersifat netral; translasi wire Gemini berada di adapter; retry/backoff di plugin terpisah.
7. **Append-only session log** sebagai sumber kebenaran yang dapat direkonstruksi.
8. **Profiles + bundles + patch overlay** untuk komposisi konfigurasi (mis. profil `dev`, `prod`, `lowmem`).

### 4.2 Stack Teknis

| Lapisan | Pilihan | Alasan |
|---|---|---|
| Bahasa | **Rust (edition 2024)** | Performa & memori terbaik; single static binary; tanpa GC pause |
| Async runtime | **tokio** (multi-thread) | Standar industri, matang, ekosistem luas |
| HTTP server | **hyper 1.x / axum 0.8** | Zero-copy, ringan, fleksibel; axum memberi ergonomi routing |
| HTTP client | **reqwest + rustls** (dengan opsi kustom `ClientHello`/JA3 via `boring`/`rustls` kustom) | TLS fingerprint impersonation adalah kebutuhan nyata (Google menilai JA3) |
| Serialisasi | **serde / serde_json** + `simd-json` opsional | Parsing `f.req` posisional & respons bersarang cepat |
| Streaming | **tokio-util `Stream`** + SSE (axum `Sse`) | Backpressure alami, delta prefix-diff |
| Storage metadata | **SQLite (rusqlite, bundled)** | Satu file, transaksional, tanpa server |
| Storage media | Filesystem + content-addressed (SHA-256) | Sederhana, hemat RAM, mudah di-backup |
| Konfigurasi | **figment** atau `config` crate + TOML | Overlay config + env |
| Observability | `tracing` + `tracing-subscriber` (JSON) | Log terstruktur, request-id |
| Plugin system | In-repo registry + trait object + `inventory`/manual registration; hot reload via `libloading` (opsional) | Reversible effect & DI tanpa overhead framework besar |
| Kompresi cache gambar | zstd (opsional) | Hemat disk |
| Test | `cargo test`, `cargo nextest`, `wiremock`, `insta` (snapshot), `oha`/`wrk` untuk beban | Deterministik + benchmark nyata |
| Build/distribusi | `cargo build --release` (musl target untuk static), `cargo-dist` atau `cross` | Single binary; Docker scratch opsional |

### 4.3 Arsitektur Komponen

```
┌─────────────────────────────────────────────────────────────────┐
│                      gemini-bridge (proses Rust)                │
│                                                                 │
│  ┌───────────── Plugin context (DI + event bus + registry) ───┐  │
│  │                                                             │  │
│  │  [http-server]  →  [openai-compat]  →  [router/model-map]   │  │
│  │        │                                     │              │  │
│  │        │                                     ▼              │  │
│  │        │                            [llm-service (netral)]   │  │
│  │        │                                     │              │  │
│  │        │                    ┌────────────────┴───────────┐  │  │
│  │        │                    ▼                            ▼  │  │
│  │        │        [adapter: gemini-web]         [adapter: X]  │  │
│  │        │           │        │         │                     │  │
│  │        │           │        │         └─ [uploads]          │  │
│  │        │           │        └─ [identity/session]          │  │
│  │        │           └─ [transport (TLS/JA3, retry)]         │  │
│  │        │                                                   │  │
│  │  [media-store]  [conversation-store]  [gallery]  [health]   │  │
│  └─────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
        ▲                                              │
        │ OpenAI-compatible REST                       ▼
   aplikasi client  ◄──── SSE stream / JSON ────  Gemini Web (upstream)
```

**Alur data — chat streaming:**
1. Client → `POST /v1/chat/completions` (stream=true).
2. Plugin `openai-compat` memvalidasi & menormalkan → objek request **immutable**.
3. Waterfall middleware: rate limit → redaksi log → resolusi model → routing adapter.
4. `llm-service` meminta stream ke adapter `gemini-web`.
5. Adapter: pastikan sesi valid (bootstrap `/app` bila perlu) → bangun `f.req` posisional → `StreamGenerate`.
6. Parser membaca baris newline-framed → ekstraksi `wrb.fr` → ambil teks kumulatif → **prefix-diff** → emit delta.
7. Delta diterjemahkan ke chunk SSE OpenAI → client.
8. Gambar/attachment yang terdeteksi → di-cache ke lokal → diganti URL bridge.
9. Terminal event: `usage`, `finish_reason`, `gemini_metadata`.

### 4.4 Integration Points

- **Inbound:** REST/SSE di `127.0.0.1:8090` (default), API key opsional (`Authorization: Bearer`), CORS opt-in.
- **Outbound:** `gemini.google.com`, `content-push.googleapis.com`, `googleusercontent.com` — melalui plugin transport yang mendukung proxy (HTTP/SOCKS5).
- **Storage:** `~/.local/share/gemini-bridge/` → `bridge.sqlite`, `media/`, `logs/`, `cookies.json` (mode 0600).
- **Auth:** cookie sesi Google disimpan di file berenkripsi-at-rest (opsional: passphrase via env `BRIDGE_SECRET`); tidak pernah ditulis ke log.
- **Kontrol:** `POST /admin/reauth`, `POST /admin/reload-plugin`, `GET /admin/status` (bind ke localhost, wajib API key admin).

### 4.5 Skema API (ringkas)

```
POST   /v1/chat/completions            # OpenAI-compatible (stream + non-stream)
POST   /v1/images/generations          # prompt + reference_images → gambar
GET    /v1/images/{id}                 # ambil gambar (cache lokal)
POST   /v1/files                       # upload referensi (multipart)
GET    /v1/files/{id}
GET    /v1/conversations               # daftar percakapan
GET    /v1/conversations/{id}/messages # riwayat
POST   /v1/conversations/{id}/branch   # cabang percakapan
POST   /v1/conversations/{id}/regenerate
GET    /v1/models                      # daftar model virtual + alias
GET    /gallery                        # JSON (format=html untuk halaman ringan)
GET    /healthz      /readyz           # kesehatan proses & sesi
GET    /metrics                        # Prometheus (opsional, off-by-default)
```

**Ekstensi non-OpenAI** (namespaced, tidak mengganggu kompatibilitas):
- Header respons `x-gemini-bridge-continuity`, `x-gemini-bridge-model`, `x-gemini-bridge-bl`.
- Field respons `gemini_metadata: { citations[], code_execution[], image_refs[], conversation_id }`.

### 4.6 Manajemen Sesi & Kredensial (titik kritis)

- **Bootstrap:** `GET /app` → regex/parse `bl`, `SNlM0e`, token upload, `f.sid`; simpan dengan TTL.
- **Rotasi `1PSIDTS`:** refresh terjadwal + on-demand saat terdeteksi stale (respons kosong/stall).
- **Auto-recovery 405:** deteksi BL basi → refresh → retry **sekali**, tanpa membocorkan error ke client.
- **Cookie expiry:** status `needs_reauth`; mode terpandu mengambil cookie dari profil browser yang sudah login (integrasi opsional dengan Camofox sebagai *cookie source*, bukan ketergantungan runtime).
- **Deteksi IP flag:** respons 302 ke `sorry/index` → status `ip_flagged` + saran proxy, tanpa retry membabi buta.

### 4.7 Security & Privacy

| Aspek | Keputusan |
|---|---|
| Bind default | `127.0.0.1` saja; ekspos jaringan harus opt-in eksplisit |
| API key | Wajib untuk bind non-localhost + endpoint `/admin/*` |
| Rahasia | Cookie & token hanya di disk (0600) / memori; **redaksi otomatis di log** (`waterfall` middleware) |
| SSRF | Referensi `reference_images[]` dari URL dievaluasi: tolak private/loopback/link-local, resolusi DNS dipin, hanya http(s) |
| Upload | Batas ukuran (default 20 MB), mime sniffing, hanya image/* |
| Data di disk | Cache gambar berisi konten dari akun; dokumentasi wajib menyebut; TTL & purge (`POST /admin/purge`) |
| Telemetri | **Nol telemetri keluar.** Tidak ada phone-home |
| Compliance | Dokumentasi menyatakan: automasi tidak resmi melanggar ToS Google; akun berisiko ditangguhkan; kredensial = akses penuh akun → jangan dibagikan |

### 4.8 Plugin System (spesifikasi minimum)

- **Trait kunci:** `Plugin` (`Id`, `Requires`, `Setup(ctx) -> Result<Disposer>`), `Service` (didaftarkan di context), `Adapter` (implementasi `llm-service`/`media-service`).
- **Context API (target):** `ctx.provide::<T>()`, `ctx.inject::<T>()`, `ctx.effect(|| ...)` (auto-dispose), `ctx.on(event, mode, handler)`, `ctx.freeze(request)`.
- **Event minimum v1:** `request.received`, `request.built`, `upstream.start`, `stream.chunk`, `stream.done`, `upstream.error`, `auth.renewed`, `plugin.loaded`, `plugin.unloaded`.
- **Manifest plugin:** TOML (nama, versi, requires, config schema) + entri kode (built-in atau dynamic lib).
- **Hot reload:** `POST /admin/reload-plugin {name}` → dispose → re-setup → verifikasi; stream aktif **tidak** terganggu (buffering per-koneksi).

### 4.9 Parameterisasi Rapuh Upstream (mitigasi)

Karena indeks `f.req` dan pohon respons dapat berubah sewaktu-waktu:

- **Schema map sebagai data, bukan konstanta tersebar.** Semua indeks (`[17]`, `[79]`, `[41]`, dst.) berada di satu file `schema/gemini-web.toml` yang dapat di-override per profil/versi BL.
- **Self-check saat startup & berkala.** Probe ringan yang memverifikasi indeks masih menghasilkan respons valid; bila gagal → mode `degraded` + peringatan eksplisit.
- **Fallback berlapis:** PIN ke BL yang diketahui → auto-deteksi BL baru → bila tetap gagal, `501` dengan pesan actionable (bukan 500).
- **Snapshot test** terhadap respons tersimpan (`insta`) agar perubahan parser terdeteksi di CI.

---

## 5. Risks & Roadmap

### 5.1 Risiko Teknis & Mitigasi

| # | Risiko | Dampak | Probabilitas | Mitigasi |
|---|---|---|---|---|
| R1 | Google mengubah skema `f.req`/pohon respons | **Tinggi** — fitur berhenti | Tinggi | Schema map sebagai data + self-check + snapshot test + rilis cepat |
| R2 | Build label (`bl`) berubah / 405 | Sedang | Tinggi | Auto-detect + refresh + retry sekali (sudah terbukti pola ini) |
| R3 | Cookie `1PSIDTS` mandek → respons kosong | Sedang | Sedang | Rotasi otomatis + deteksi stall + status `needs_reauth` |
| R4 | IP di-flag (DC/VPS/docker NAT) | Tinggi | Sedang | Dukungan proxy, deteksi dini, dokumentasi penyebab |
| R5 | **Pelanggaran ToS → akun ditangguhkan** | **Kritis untuk user** | Sedang–Tinggi | Dokumentasi tegas di README; rekomendasi akun terpisah; tidak ada klaim "aman dari ban" |
| R6 | Pipeline image generation gagal diekstrak (indeks gambar tak stabil) | Tinggi (fitur inti) | Sedang | Ekstraktor berbasis pola URL (bukan indeks tetap) + test gambar nyata di CI + fallback ke regex `googleusercontent.com/image/` |
| R7 | Tool calling emulasi tidak stabil | Sedang | Sedang | Validator + fallback teks + metrik parse sukses |
| R8 | Streaming terputus / delta tidak sinkron saat snapshot berubah | Sedang | Sedang | Prefix-diff dengan deteksi non-prefix → reset aman, buffer per koneksi, tidak retry buta |
| R9 | TLS fingerprint diblokir di beberapa region | Tinggi | Rendah–Sedang | Opsi profil fingerprint (Chrome/Firefox/Safari) + proxy |
| R10 | Rust + plugin dinamis menambah kompleksitas awal | Sedang (timeline) | Sedang | Mulai dengan **built-in registry** (plugin statis, zero unsafe), dynamic loading sebagai fase lanjut |
| R11 | Drift saat snapshot respons kumulatif pada model thinking | Rendah | Sedang | Test khusus model thinking; toleransi rewrite eksplisit |

### 5.2 Roadmap Bertahap

#### Fase 0 — Bootstrap (1–2 minggu)
- Skeleton Rust workspace, plugin context minimal, config TOML, logging tracing.
- Plugin `identity` (bootstrap `/app`, parse `bl`/`SNlM0e`) + `transport` (reqwest + opsi TLS profil).
- Plugin `adapter:gemini-web` untuk chat **non-stream**.
- CLI: `gemini-bridge auth login` (impor cookie), `gemini-bridge doctor` (cek sesi & konektivitas).
- **Exit criteria:** chat non-stream berhasil end-to-end lewat `POST /v1/chat/completions`.

#### Fase 1 — MVP (3–5 minggu) — *rilis pertama*
- Streaming SSE + prefix-diff.
- Image generation (`/v1/images/generations`) + ekstraksi URL + cache lokal + `GET /v1/images/{id}`.
- Upload referensi (`/v1/files`) + integrasi fileRef.
- `/healthz`, `/readyz`, `/admin/status`, rotasi `1PSIDTS`, auto-recovery 405.
- **Exit criteria:** KPI #4, #5 (chat + image + files) lulus; 7 hari jalan tanpa intervensi.

#### Fase 2 — v1.1 (3–4 minggu)
- Percakapan persisten (conversationId/responseId) + branching + regenerate.
- Tool calling emulasi + validator.
- Gallery (JSON + HTML) + purge/TTL.
- Middleware waterfall (rate limit, redaksi, audit log).
- **Exit criteria:** E1 ≥ 95%, E3 memenuhi KPI #1/#2.

#### Fase 3 — v2.0 (4–6 minggu)
- Code execution & citation surfacing (`gemini_metadata`).
- Video (eksperimental, off-by-default) + `501` yang rapi.
- Hot reload plugin via dynamic loading + `profiles/bundles/patch overlay`.
- Adapter kedua (proof of "everything is a plugin") — mis. adapter untuk provider lain, atau adapter mock untuk testing.
- Observability: `/metrics`, dashboard status ringan.
- **Exit criteria:** KPI #3, #6, #7 lulus; satu adapter non-Gemini berjalan tanpa mengubah core.

### 5.3 Dependensi & Prasyarat

- Akun Google dengan akses Gemini Web (sudah ada: `hoperenezena@gmail.com`; **catatan: paket AI Pro terlihat sudah expired** per observasi live — artinya mode free tier, dengan limit lebih ketat pada model Pro/thinking).
- VPS/host dengan IP yang tidak di-flag (verifikasi di Fase 0 dengan `doctor`).
- Opsional: proxy residensial bila IP datacenter bermasalah.

### 5.4 Pertanyaan Terbuka (butuh keputusan sebelum Fase 1)

1. **Nama & lisensi final** project (MIT? Apache-2.0? privat?).
2. **Kebijakan akun:** apakah memakai akun utama Afi, atau sebaiknya akun Google khusus (rekomendasi kuat: khusus).
3. **Audio/TTS & video:** apakah benar-benar diprioritaskan di v2.0, atau ditunda sampai adapter inti stabil?
4. **UI gallery:** cukup halaman HTML statis yang disajikan binary, atau perlu SPA kecil?
5. **Distribusi plugin pihak ketiga:** perlu ABI stabil + dynamic loading sejak v1, atau cukup built-in dulu?

---

## 6. Lampiran

### 6.1 Referensi
- `ikhsan3adi/gemini-web2api` — Go port, referensi wire protocol & resilience (MIT).
- `HanaokaYuzu/Gemini-API`, `Zai-Kun/gemini-webapi` — referensi parsing respons & rotasi cookie.
- `deepseek-ai/deepseek-harness` + Cordis — inspirasi arsitektur *everything is a plugin*, DI, event bus mode, profiles/bundles.
- Laporan eksplorasi internal: `~/reports/gemini-web-exploration.md` (mencakup endpoint map, `f.req` indices, upload flow, temuan live session, celah gemini-web2api).

### 6.2 Temuan Live yang Sudah Terverifikasi
- Chat send **benar-benar** memakai `StreamGenerate` (bukan hanya batchexecute) — dikonfirmasi via `performance.getEntriesByType('resource')` pada sesi login nyata.
- `bl` live: `boq_assistant-bard-web-server_20260928.08_p0`; `f.sid` live: `514600432520438493`.
- 18 rpcid batchexecute teramati pada page-load.
- Round-trip uji "PONG-42" berhasil → sesi & pipeline valid end-to-end.

### 6.3 Cakupan vs. `gemini-web2api` (diferensiasi)
| Aspek | gemini-web2api | gemini-bridge (target) |
|---|---|---|
| Bahasa | Go | **Rust** |
| Image generation output | ❌ tidak ada | ✅ ekstraksi + cache + endpoint |
| Kontinuitas percakapan | ❌ tiap request baru | ✅ conversationId + branch |
| Rotasi `1PSIDTS` | ❌ manual | ✅ otomatis |
| Arsitektur plugin | ❌ monolitik berlapis | ✅ everything-is-a-plugin |
| Code execution & citation | ❌ dibuang | ✅ di-surface |
| Gallery & manajemen media | ❌ | ✅ |
| Video | ❌ | ⚠️ eksperimental |

---

*Dokumen ini adalah draft. Bagian 5.4 (Pertanyaan Terbuka) perlu keputusan sebelum masuk Fase 1.*
