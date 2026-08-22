# Code signing policy

Status: preparing the SignPath Foundation application. No current QuickNote artifact claims a SignPath Foundation signature.

Upon acceptance, **Free code signing provided by [SignPath.io](https://signpath.io/), certificate by [SignPath Foundation](https://signpath.org/)**.

## Project identity

- Project: QuickNote
- Source repository: <https://github.com/eelevenn/quicknote>
- License for QuickNote-owned source: [MIT](LICENSE)
- Privacy policy: [PRIVACY.md](PRIVACY.md)
- Third-party components: [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)

SignPath Foundation owns the certificate used by its open-source program. After acceptance, Windows may therefore show `SignPath Foundation` as the verified publisher rather than the repository owner's personal name.

## Team roles

- Committer and reviewer: [eelevenn](https://github.com/eelevenn)
- Signing approver: [eelevenn](https://github.com/eelevenn)

All people assigned to these roles must keep multi-factor authentication enabled for GitHub and SignPath access. Contributions from people without commit access require review before merge. Every release signing request requires explicit approval by the signing approver.

## Build and signing rules

- Release artifacts must be built by the repository's pinned GitHub Actions workflow from a version tag whose source is public and reviewable.
- Build scripts, dependency locks and workflow changes are part of the reviewed source.
- Only binaries built from QuickNote-owned source may be submitted under the QuickNote signing policy.
- Upstream artifacts must not be signed as though they were produced by QuickNote.
- Product name and version metadata must agree across every artifact in one release.
- SHA-256 hashes and signature verification results must be published with each signed release.
- Every signing request requires manual approval; local or unverifiable binaries are not eligible.

The intended signed artifacts are the QuickNote Windows executable and the per-user MSI installer. QuickNote v0.1.0 does not distribute a speech model, transcription runtime or sidecar.

## Reporting concerns

Please report suspected malware, signing-policy violations or compromised releases through a private GitHub security advisory at <https://github.com/eelevenn/quicknote/security/advisories/new>. Do not include sensitive details in a public issue.
