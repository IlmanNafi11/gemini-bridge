# Changelog

All notable user-facing changes to Gemini Bridge are documented here.

## [Unreleased]

### Fixed

- Include Gemini Web's anti-XSRF `at` token in StreamGenerate form submissions.
- Align integration response fixtures with the current array-based Gemini Web schema, restoring chat, metadata, streaming, and 405-recovery tests.

## [0.1.0] - 2026-10-02

### Added

- OpenAI-compatible chat completions backed by authenticated Gemini Web sessions.
- Health, readiness, protected administration, metrics, media, image, gallery, upload, tool-calling, and conversation-store surfaces.
- Locked static Linux release archive with checksums, SPDX and CycloneDX SBOMs, provenance attestations, systemd unit, and container image.

### Security

- API-key enforcement for non-loopback listeners and protected administrative routes.
- Hardened container and systemd defaults, encrypted credential storage, secret redaction, request limits, and auditable release gates.

### Known limitations

- A live Gemini Web credential is required for upstream requests.
- The shipped binary rejects enabled video generation because no production video adapter is included.

[0.1.0]: https://github.com/IlmanNafi11/gemini-bridge/releases/tag/v0.1.0
