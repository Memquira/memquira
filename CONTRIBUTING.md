# Contributing

Thanks for your interest in Memquira. Bug reports, documentation fixes and focused pull requests are welcome.

## Before you start

- Search existing issues first. For a new feature or a change in behaviour, open an issue to discuss it before
  writing code.
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
- CI (format, clippy, tests, dependency and secret checks) must pass.

## Licensing of contributions

Memquira is licensed under the GNU Affero General Public License v3.0 or later. By submitting a contribution you
agree that it is licensed under the same terms, and you confirm that you have the right to submit it.
