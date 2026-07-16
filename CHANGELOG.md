# Changelog

CLIME の主な変更をこのファイルに記録する。

## [0.1.0] - 2026-07-16

### Added

- 物理 Caps Lock に安全側のIME確定判定を割り当てる共通EngineとCompositionTracker。
- Windowsの低レベルフック、IME状態取得、キー注入、HKCU自動起動、doctor。
- macOSのhidutilリマップ、CGEventTap、TIS判定、LaunchAgent自動起動、TCC/hidutil doctor。
- Linuxのevdev監視、uinput注入、fcitx5/IBus判定、systemd/XDG自動起動、権限/XKB doctor。
- Windows x64、macOS universal、Linux x64 のタグ駆動GitHub Releasesワークフロー。
- 3OS向けのインストール、権限設定、自動起動登録手順。
