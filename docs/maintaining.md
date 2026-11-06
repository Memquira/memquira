# Maintaining Memquira

How issues and pull requests are handled, and how the repository is protected. Contributors should read
[CONTRIBUTING.md](../CONTRIBUTING.md) first.

## Labels

| Label | Meaning |
|---|---|
| `needs-triage` | New issue, not yet reviewed. Added by every issue form. |
| `needs-info` | Waiting for the reporter (reproduction steps, version, logs). |
| `bug`, `enhancement`, `question`, `documentation` | The kind of issue. |
| `good first issue`, `help wanted` | Open for outside contributors. |
| `duplicate`, `invalid`, `wontfix` | Reason an issue was closed without a change. |

## Issue triage

1. Review new issues within 5 working days. Remove `needs-triage` once the issue is understood and labelled.
2. If an issue contains a secret (API key, token, password, credentials in a URL) or personal data, edit it out
   immediately and ask the reporter to rotate the secret. Editing does not remove the original from the issue's edit
   history; an organization owner can delete the issue entirely.
3. If an issue reports a vulnerability, do not discuss it in public. Ask the reporter to resubmit it through
   [private vulnerability reporting](https://github.com/Memquira/memquira/security/advisories/new), then hide or
   delete the public issue. Handle it as described in [SECURITY.md](../SECURITY.md).
4. Bugs: reproduce with the reported version, then label `bug`, or `needs-info` with a concrete question. Close
   `needs-info` issues after 14 days without a reply; they can be reopened.
5. Feature requests: discuss the problem before the solution. Label `help wanted` or `good first issue` only when the
   expected change is clear and small enough for a first pull request.
6. Close duplicates with a link to the original issue.

## Pull requests

- Every change, including the maintainers' own, goes through a pull request; direct pushes to `main` are rejected.
- A pull request can be merged when all eight CI checks pass, the branch is up to date with `main`, and every review
  conversation is resolved. Squash-merge with a conventional title (`fix: ...`, `feat: ...`, `docs: ...`).
- Workflows from first-time contributors run only after a maintainer approves them. Review the diff, especially
  changes under `.github/`, scripts and the Dockerfile, before approving.
- Only maintainers with write access can merge. Outside contributors work from forks.

## Repository rules

Configured in the repository settings under Rules → Rulesets, with an empty bypass list, so the rules apply to
administrators too.

- **`main` (branch ruleset):** pull request required (0 approvals while there is a single maintainer), conversation
  resolution required, the eight CI checks required with the branch up to date, force pushes and deletion blocked.
- **`v*` (tag ruleset):** release tags cannot be moved (updated), force-pushed or deleted. Creating a new tag, for
  example by publishing a GitHub Release, is allowed. A wrong release is fixed with a new patch version, not by moving
  a tag.

### Requiring code-owner review when a second maintainer joins

1. Give the new maintainer the **Write** or **Maintain** role (not Admin) in Settings → Collaborators and teams.
2. Add `.github/CODEOWNERS` in a pull request, for example `* @first-maintainer @second-maintainer`, with paths for
   areas a specific maintainer owns.
3. In the `main` ruleset, set **Required approvals** to 1 and enable **Require review from Code Owners**, **Dismiss
   stale pull request approvals when new commits are pushed** and **Require approval of the most recent reviewable
   push**.
4. From then on, each maintainer's pull requests need the other maintainer's approval. Keep the bypass list empty.

## Releases

1. Update `CHANGELOG.md` (version and release date) in a pull request and merge it after CI passes.
2. Publish a GitHub Release from `main`, creating the tag `vX.Y.Z` on the merged commit.
3. Check that the tag points to the intended commit; the tag ruleset then keeps it fixed.
