# Changelog

CL4SE の主な変更をこのファイルに記録する。

## [Unreleased]

## [1.0.4] - 2026-07-19

### Changed

- LPの導入コマンド、リリースリンク、表示バージョンをv1.0.4へ更新。

### Fixed

- WindowsのIME状態取得を、実際のフォーカス窓に対応する既定IMEウィンドウへの、タイムアウト付き `IMC_GETOPENSTATUS` 照会へ一本化。別プロセスの入力コンテキストを直接取得する経路と、IMEによって応答しない変換モード照会を廃止し、IME ON時の変換確定とIME OFF（タスクバー表示「A」）時の安全改行が、ともに既知状態として判定されるように修正。
- IME状態を取得できない場合や照会中に前面窓が変わった場合は、従来どおり `Unknown` としてキーを注入しないフェイルセーフを維持。

## [1.0.3] - 2026-07-19

### Added

- 最新のGitHub安定版を取得し、SHA-256検証・安全な停止・置換・再開を行う `cl4se update` コマンド。

### Changed

- LPの導入コマンドをv1.0.3へ更新し、既存インストールがある場合も新しいバイナリへ置き換えるように変更。各OS欄に `update` コマンドを追加。

### Fixed

- Windowsでフォーカス窓のIMM入力コンテキストを直接照会し、IMEウィンドウ経由の変換モード取得が失敗するアプリでも、タスクバー表示が「A」の英数字モードを `No` と判定して安全改行できるように修正。取得結果が競合する場合は従来どおり `Unknown` に倒す。

## [1.0.2] - 2026-07-19

### Fixed

- WindowsでIMEのopen状態だけでなく変換モードも確認し、タスクバー表示が「A」の英数字モードを非変換状態として安全改行できるように修正。

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
