# Privacy policy

QuickNote is a local-first Windows application. This policy describes the MVP behavior represented by this repository.

## Local data

- Notes, reminders, settings, backups and downloaded transcription assets are stored under `%LOCALAPPDATA%\QuickNote`.
- Microphone audio is processed on the user's device. QuickNote does not upload recorded audio or transcription text.
- QuickNote does not include telemetry, advertising, analytics or automatic crash reporting.
- Exported files are written only to a location explicitly selected by the user.

## Network access

QuickNote does not transfer information to another networked system unless the user explicitly requests an operation that requires network access.

When the user chooses to install the optional local transcription package, QuickNote downloads pinned sherpa-onnx runtime and SenseVoice model archives from the URLs recorded in `crates/quicknote-app/assets/transcription-package.json`. The download host can receive ordinary connection metadata such as the user's IP address and user agent under that host's privacy policy. Note contents and recorded audio are not included in these requests.

GitHub's privacy statement applies when the current package URLs resolve to GitHub Releases: <https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement>.

## Uninstallation

The MSI uninstaller removes the installed application and its Windows shell registrations. User data is preserved by default so that reinstalling QuickNote can recover it. A user can permanently remove local data by deleting `%LOCALAPPDATA%\QuickNote` after uninstalling.

## Changes and contact

Material privacy changes will be documented in this file and in the corresponding release notes. Questions can be filed in the public issue tracker: <https://github.com/eelevenn/quicknote/issues>.
