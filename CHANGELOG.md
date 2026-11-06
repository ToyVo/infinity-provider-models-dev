# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0] - 2026-11-06

### Added

- `INFINITY_DEFAULT_MODEL` env var to pin the global default model. Set it to a
  full `<backend>/<model>` id (e.g. `groq/openai/gpt-oss-120b`) and that model is
  advertised first from `list_models`, so infinity picks it as the default.
  A known-but-unlisted id is advertised first as a passthrough entry (with a
  warning in the daemon log); an unknown backend prefix warns and keeps the
  normal order. Unset or empty preserves the previous preset-order behavior.

## [0.2.2] - 2026-11-06

### Fixed

- Sync the latest GLM 5.3 and GLM 5.3 flash models from z.ai.

## [0.2.1] - 2026-11-05

### Fixed

- Review findings: timeouts, framing, stream contract.

## [0.2.0] - 2026-11-05

### Added

- OpenCode console and Go backends with live catalogs.
