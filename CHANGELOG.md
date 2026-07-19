# Changelog

CL4SE の主な変更をこのファイルに記録する。

## [Unreleased]

## [1.0.1] - 2026-07-19

### Added

- GitHub PagesによるLP公開ワークフローと、動作報告・不具合報告用のIssue Forms。
- MITライセンスとCargoパッケージのリポジトリ情報。

### Changed

- LPへフェイルセーフ設計とフィードバックの案内を追加し、画面幅に応じた見出しの改行位置を改善。

### Fixed

- Windowsのログイン時自動起動がコンソール付きのフォアグラウンド実行になる問題。
- WindowsでIMEがOFFのとき、任意設定の安全改行が動作しない問題。フォーカス窓で取得不能な場合だけ同じGUIスレッドの前面窓へフォールバックし、状態不明時は引き続き何も注入しない。

## [1.0.0] - 2026-07-17

### Changed

- 製品名、CLI名、設定保存先、自動起動ID、リリース成果物名を CL4SE / `cl4se` に統一。

### Added

- 設定の表示・変更と稼働中プロセスの安全な自動再起動を行う `cl4se setting` コマンド。
- 冪等なバックグラウンド起動・再開を行う `cl4se start` と、通常クリーンアップ完了まで待つ `cl4se stop`。
- 非変換中のCaps LockでShift+Enterを注入する、既定OFFの `idle_action = "shift_enter"`。
- 物理 Caps Lock に安全側のIME確定判定を割り当てる共通EngineとCompositionTracker。
- Windowsの低レベルフック、IME状態取得、キー注入、HKCU自動起動、doctor。
- macOSのhidutilリマップ、CGEventTap、TIS判定、LaunchAgent自動起動、TCC/hidutil doctor。
- Linuxのevdev監視、uinput注入、fcitx5/IBus判定、systemd/XDG自動起動、権限/XKB doctor。
- Windows x64、macOS universal、Linux x64 のタグ駆動GitHub Releasesワークフロー。
- 3OS向けのインストール、権限設定、自動起動登録手順。

### Fixed

- Windowsで設定変更後に再起動用の子プロセスがバックグラウンドへ残り、起動元ターミナルの `Ctrl+C` で停止できなくなる問題。再起動を同じフォアグラウンドプロセス内の安全な再初期化へ変更。
