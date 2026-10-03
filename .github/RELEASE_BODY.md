# Gemini Bridge 0.1.0

This release provides an OpenAI-compatible Gemini Web bridge and a reproducible, statically linked Linux x86_64 archive. It includes the MIT license, example configuration, operations guide, systemd unit, SHA256SUMS, CycloneDX and SPDX SBOMs, and provenance attestations.

## Install

Download the archive and checksum manifest from this release. Verify all files with `sha256sum --check --strict SHA256SUMS`, then inspect/install with the included scripts or extract the archive manually. The bundled systemd unit uses `/usr/local/bin/gemini-bridge`; the installer therefore accepts only `/usr/local` as its prefix.

A non-loopback listener requires a strong API key. A live Gemini Web credential is required to make upstream requests. The shipped binary does not provide video generation; enabling it is rejected.

See the bundled `docs/operations.md` for configuration, storage, security, upgrade, and rollback details.