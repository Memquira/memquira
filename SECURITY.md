# Security policy

Memquira is early-stage software (0.x) and has not been independently audited.

## Reporting a vulnerability

Please do **not** open a public issue for a security problem. Use the private
[**Report a vulnerability**](https://github.com/Memquira/memquira/security/advisories/new) form on this repository's
**Security** tab (GitHub private vulnerability reporting) and include:

- the version (`GET /api/v1/version`) and deployment shape (auth mode, storage backends, providers);
- steps to reproduce and the impact you observed (for example cross-tenant access, key disclosure, authentication
  bypass, data left behind after deletion);
- whether the issue is already public.

We aim to acknowledge reports within 5 working days and will keep you informed while a fix is prepared. Only the
latest release receives security fixes.

## Scope

In scope: the `memcore-api` server, `memcore-maintenance`, the engine, storage and provider crates, the Docker
image and the Compose files in this repository.

Out of scope: vulnerabilities in third-party model providers or databases, issues in your own gateway or proxy, and
deployments running `MEMCORE_AUTH_MODE=dev` (a single shared key intended for local development only).

Hardening guidance for operators: [docs/security.md](docs/security.md).
