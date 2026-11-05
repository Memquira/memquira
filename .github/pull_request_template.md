## Summary

<!-- What does this change do, and why? -->

## Linked issue

<!-- For example "Closes #123". Open an issue first for a new feature or a change in behaviour. -->

## Tests

<!-- How did you verify the change? Tick what you ran. -->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`, or the affected crates: <!-- list them -->
- [ ] Tests added or updated for behaviour changes; no test calls a paid model provider

## Checklist

- [ ] No secrets, API keys, tokens, passwords, credentials or real user data are included in code, tests,
      fixtures, logs or screenshots.
- [ ] Documentation and `openapi/memcore.openapi.json` are updated if configuration or the API changed.
- [ ] No released database migration was edited.

Contributions are licensed under the Apache License 2.0 ([CONTRIBUTING.md](https://github.com/Memquira/memquira/blob/main/CONTRIBUTING.md#licensing-of-contributions)).
