# Contributing

Thanks for your interest in Memquira. Bug reports, documentation fixes and focused pull requests are welcome.

## Before you start

- Search existing issues first. For a new feature or a change in behaviour, open an issue to discuss it before
  writing code. [New issues](https://github.com/Memquira/memquira/issues/new/choose) use a form: bug report, feature
  request, or documentation and usage question.
- Report security problems privately ([SECURITY.md](SECURITY.md)), never in a public issue.
- Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).

## Development setup

See [docs/development.md](docs/development.md). In short:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Pull requests

- Keep each pull request to one change, with a clear description of what and why.
- Add or update tests for behaviour changes. Tests must not call paid model providers or need private
  credentials; use the mock providers and in-process fakes.
- Update the documentation and `openapi/memcore.openapi.json` when you change configuration or the API
  ([docs/development.md](docs/development.md#openapi-document)).
- Never edit a released database migration; add a new one.
- Do not commit secrets, real customer data, or third-party datasets without a licence that allows
  redistribution.
- Every change, including the maintainers' own, reaches `main` through a pull request. All eight CI checks
  (format, clippy, tests, integration, feature builds, dependency and licence checks, secret scan, Docker image) must
  pass and review conversations must be resolved before it is merged. How issues and pull requests are triaged:
  [docs/maintaining.md](docs/maintaining.md).

## Licensing of contributions

Memquira is licensed under the Apache License, Version 2.0. Unless you state otherwise, any contribution you
intentionally submit for inclusion is licensed under the same terms, without any additional terms or conditions
(section 5 of the licence), and by submitting it you confirm that you have the right to do so.
