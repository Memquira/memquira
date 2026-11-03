# Health Route Plan

This plan is intentionally lightweight for the workspace scaffold phase.

## Next Step Scope

- Add Axum and Tokio dependencies to `memcore-api`.
- Introduce a minimal router bootstrap.
- Add `GET /health` returning JSON:
  - `status` (e.g. `"ok"`)
  - `service` (e.g. `"memcore-api"`)
- Add a basic route test for health response status and payload shape.

## Out of Scope for This Phase

- readiness checks
- database connectivity checks
- provider checks
- full middleware stack
