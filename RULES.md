# RULES.md — Gemini Bridge

Dokumen ini adalah aturan kerja yang mengikat untuk repository `gemini-bridge`.
Setiap perubahan — oleh manusia maupun agen — MUST mematuhi aturan di bawah ini.

## 1. Branch

Repository terdiri dari **3 branch**, masing-masing MUST digunakan sesuai scope dan tujuannya:

| Branch | Scope | Penggunaan |
| --- | --- | --- |
| `dev` | development | Segala pengembangan dan perbaikan |
| `staging` | environment | Internal testing |
| `main` | production | Production |

- Segala pengembangan dan perbaikan MUST berada di branch `dev`.
- Branch `staging` MUST digunakan untuk internal testing.
- Branch `main` MUST digunakan untuk production.

## 2. CI

### 2.1 Branch `staging` — trigger: pull request

1. Kembangkan/perbaiki issue di branch `dev`.
2. Selesai → push ke `dev` dan ajukan pull request ke `staging`.
3. Merge ke `staging` **hanya boleh** jika **lint, build, test (unit dan integration), dan formatting pass 100%**; **tolak merge jika fail**.
4. Jika pass → lakukan **auto deployment** sesuai ketentuan [Deployment](#7-deployment).

### 2.2 Branch `main` — trigger: tag

- Release dijalankan saat tag dibuat, sesuai ketentuan [Deployment](#7-deployment).
- Tidak ada proses lint, build, test (unit dan integration), dan formatting check di branch `main`, karena sudah dilalui pada branch `staging`.

## 3. Security

- **DILARANG KERAS** commit credential, IP, atau data sensitif.

## 4. Git

- Selalu commit menggunakan skill `git-workflow-and-versioning`.

## 5. Quality Gate

- Setiap selesai melakukan perbaikan pada code (bukan docs atau sejenisnya), MUST dipastikan **lint, build, test (unit dan integration), dan formatting pass 100%**.

## 6. Workflow Pengembangan

- Pengembangan sistem MUST mematuhi workflow development (SDLC) dari skill `agent-skills@addy-agent-skills`.
- Pengerjaan task, proses shipping, code-review, dan security analysis MUST sesuai dengan scope `SPEC.md`, `tasks/plan.md`, dan `tasks/todo.md` yang sedang berjalan; dilarang memperbaiki/execute di luar scope.
- `SPEC.md` adalah SPEC utama; detail setiap modul MUST menjadi spec per modul sesuai ketentuan skill `agent-skills@addy-agent-skills`.
- Segala sesuatu yang memerlukan decision atau bersifat kritis MUST selalu diputuskan melalui MCP `sequential-thinking`.
- Index project, eksplorasi/pemahaman codebase, search code, graph, dan sejenisnya MUST menggunakan MCP/skill `codebase-memory`.

## 7. Deployment

- Deployment ke staging environment dan production environment dilakukan melalui docker image hasil build di CI.
- Gunakan MCP `dokploy` untuk setup staging environment dan production environment untuk deployment dan release.
