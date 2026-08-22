# Third-party notices

The MIT license in [LICENSE](LICENSE) applies only to source code and documentation owned by QuickNote contributors. Third-party software and model assets remain under their respective licenses.

## Optional local transcription components

QuickNote's main MSI does not contain the optional transcription runtime or model. The user can explicitly request their separate download, and the application verifies the pinned size and SHA-256 values before activation.

| Component | Upstream | Declared license | Distribution treatment |
| --- | --- | --- | --- |
| sherpa-onnx runtime | <https://github.com/k2-fsa/sherpa-onnx> | Apache License 2.0 | Downloaded separately; upstream notices must be retained |
| SenseVoice-Small model | <https://github.com/QwenAudio/SenseVoice> | FunASR Model Open Source License 1.1 for model weights | Downloaded separately; attribution and model name must be retained |

The exact URLs, versions and checksums used by the MVP are recorded in `crates/quicknote-app/assets/transcription-package.json`. QuickNote does not relicense these assets as MIT and must not sign them with the QuickNote SignPath policy.

## Rust and Windows build dependencies

Rust crates, Slint, SQLite, WiX and their transitive dependencies retain the licenses shipped by their respective authors. A complete machine-generated dependency inventory and the required license texts must be reviewed and attached to each release before the release gate can pass.

This notice is an attribution aid, not a substitute for the upstream license texts or legal review.
