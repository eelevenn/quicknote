# Privacy policy

QuickNote is a local-first Windows application. This policy describes the MVP behavior represented by this repository.

## Local data

- Notes, reminders, settings and backups are stored under `%LOCALAPPDATA%\QuickNote`.
- QuickNote does not include telemetry, advertising, analytics or automatic crash reporting.
- Exported files are written only to a location explicitly selected by the user.

## Network access

QuickNote v0.1.0 does not transfer application data to another networked system. It contains no telemetry, update checker, model downloader or audio network path.

## Uninstallation

The MSI uninstaller removes the installed application and its Windows shell registrations. User data is preserved by default so that reinstalling QuickNote can recover it. A user can permanently remove local data by deleting `%LOCALAPPDATA%\QuickNote` after uninstalling.

## Changes and contact

Material privacy changes will be documented in this file and in the corresponding release notes. Questions can be filed in the public issue tracker: <https://github.com/eelevenn/quicknote/issues>.
