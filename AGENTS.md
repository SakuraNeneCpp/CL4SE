# AGENTS.md — CLIME 実装エージェント向けガイド

このファイルは実装エージェント(Codex)への常設指示と、人間のオペレーターが使うプロンプト集で構成される。**仕様の単一情報源は [README.md](README.md)**。本ファイルと仕様が矛盾したら README.md が正、ただし安全ルール(§2)はすべてに優先する。

## 1. あなたの役割

- あなたは CLIME(Caps Lock に IME確定を割り当てる常駐ツール、Rust製)の実装担当。設計は README.md に確定済み。
- 仕様の変更・拡大はしない。仕様の欠落・矛盾・技術的に不可能な点を見つけたら、**勝手に仕様を発明せず**、実装を止めて報告する(PR説明や出力に「仕様課題」として明記)。
- 1マイルストーン = 1ブランチ = 1PR。マイルストーンを跨いだ先回り実装はしない。

## 2. 安全ルール(絶対厳守)

キーボードを乗っ取るツールなので、バグはユーザーの入力環境を破壊する。以下は交渉不可:

1. **抑止してよいキーは物理 Caps Lock のみ**。他のキーを抑止・改変・遅延するコードを書かない。観測(読み取り)は可。
2. **注入してよいキーは確定キー(Enter、または README §1.3 で解決された Ctrl+M の一連)と、パススルー用 CapsLock のみ**。
3. **フェイルセーフ**(README §1.2): 判定が不確実なときは必ず「何もしない」に倒す。`ImeGuess::Unknown` を `Yes` 扱いするコードは書かない。
4. **ループ防止**: 注入イベントには必ずマーカーを付け、自前のフック/トラッカーで無視する。実装したら必ずテストまたは手動確認項目に含める。
5. **クリーンアップ保証**: SIGTERM / Ctrl+C / パニック時に、フック解除・macOS hidutil リマップ復元・uinput デバイス破棄が実行されること。パニックは境界で捕捉してクリーンアップ後に異常終了する。
6. フックコールバック/イベントタップ内で確保・ブロッキング・パニックし得る処理をしない(チャネルでワーカースレッドへ逃がす)。
7. `unsafe` は `src/platform/` 配下のみ。各 `unsafe` ブロック直前に安全性根拠のコメントを書く。

## 3. ハーネス(検証コマンド)

「完了」を宣言する前に、以下がすべて成功していること。実行できなかったものは**成功と偽らず**、実行不能だった旨と理由を報告する。

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo check --target x86_64-pc-windows-msvc
cargo check --target aarch64-apple-darwin
cargo check --target x86_64-unknown-linux-gnu
```

環境に関する注意:

- あなたの実行環境は通常 Linux コンテナ。**Windows/macOS の実挙動テストは不可能**であり、それを求められてもいない。代わりに (a) クロスターゲットの `cargo check` を通す、(b) core 層をモックで徹底的にユニットテストする、(c) 実機確認が必要な点を README §5.5 の手動テスト表に対応づけて PR に列挙する。
- `rustup target add` がネットワーク制約で失敗する場合は、ホストターゲットの check/test のみ実行し、その旨を報告する。
- 動作確認をでっち上げない。「〜のはず」と「確認済み」を区別して書く。

## 4. コーディング規約

- Rust stable / edition 2021 以降。`cargo fmt` 既定スタイル。clippy 警告ゼロ。
- core 層(`src/core/`)は OS API・`unsafe`・グローバル状態禁止。すべての公開型・遷移はユニットテスト対象。
- platform 層は README §2.2 のトレイトを実装する薄い層に保ち、判定ロジックを持ち込まない。
- エラーは `anyhow::Result`(アプリ境界)+ `thiserror`(ライブラリ的部分)。`unwrap()`/`expect()` は初期化時とテスト以外で禁止。
- ログ: 状態遷移・判定結果・注入は `debug`、起動/終了/権限問題は `info`/`warn`。キー入力の内容(どの文字を打ったか)は**いかなるレベルでもログに残さない**(キーロガー化防止。Caps Lock・Enter 等の制御キー種別のみ可)。
- コミットは Conventional Commits(`feat:`, `fix:`, `test:`, `ci:` …)。
- 依存クレートの追加は README §4 の表にあるもの + 明白に必要な dev-dependencies のみ。それ以外は理由を添えて提案として報告。

## 5. 進捗管理

- 各マイルストーン完了時、下の表の該当行を更新してコミットに含める。

| マイルストーン | 状態 | 備考 |
|---|---|---|
| M0 スキャフォールド | 完了 | CLI骨格・設定・platformスタブ・CI |
| M1 core層 | 完了 | Engine・CompositionTracker・確定キー解決・シナリオテスト |
| M2 Windows | 完了 | フック・IME判定・キー注入・自動起動・doctor |
| M3 macOS | 完了 | hidutil・CGEventTap・TIS・キー注入・自動起動・doctor |
| M4 Linux | 未着手 | |
| M5 リリース整備 | 完了 | release workflow・doctor案内・インストール手順・CHANGELOG |

---

## 6. プロンプト集(人間のオペレーター用)

以下を順番に Codex へ投入する。各プロンプトは AGENTS.md(本ファイル)と README.md が読まれている前提で書かれている。

### 共通前文(毎回先頭に付ける)

```text
リポジトリの AGENTS.md(規約・安全ルール・ハーネス)と README.md(仕様)に従って作業してください。
安全ルール(AGENTS.md §2)は仕様より優先です。完了報告には、実行したハーネスコマンドと結果、
実機でしか確認できない項目(README §5.5 のT番号で参照)を必ず含めてください。
```

### M0 — スキャフォールド

```text
マイルストーンM0を実装してください。ブランチ名: m0-scaffold

- README §2.1 のディレクトリ構成で Cargo プロジェクトを作成(バイナリ名 clime)。
- CLI(clap derive): run / install-autostart / uninstall-autostart / doctor / --version(README §1.6)。
  すべてスタブでよいが、run はconfig をロードしてログ初期化まで行い「backend not implemented」で終了する。
- config.rs: README §1.5 のスキーマを serde で実装。ファイル不在時は既定値+初回生成。既定値のユニットテストを書く。
- platform/mod.rs: README §2.2 のトレイトと ImeGuess / ImeSnapshot / CommitKey / ObservedEvent / Decision を定義。OS別モジュールは空実装(todo!ではなくErr返し)。
- .github/workflows/ci.yml: 3OSマトリクスで fmt → clippy → test → build(README §5.3)。
- .gitignore(target/ 等)。

受け入れ基準: ハーネス全コマンド成功。clime --version と clime doctor(「未実装」表示でよい)がホストで動作。
```

### M1 — core層(Engine + CompositionTracker)

```text
マイルストーンM1を実装してください。ブランチ名: m1-core

- src/core/tracker.rs: README §1.4 の状態機械 CompositionTracker。時刻は Instant を直接呼ばず
  引数またはClockトレイトで注入し、タイムアウトをテスト可能にする。
- src/core/mod.rs: Engine。ObservedEvent と ImeStateProvider(ImeSnapshot)から Decision を返す(README §1.3 の判定表)。
  TriggerKeyDown{shift:true} かつ shift_passthrough → PassThroughCapsLock。other_mods(Ctrl/Alt/Win/Cmd)
  押下中は何もしない。キーリピート無視。注入イベント(マーカー付き)を Engine が受け取った場合の扱い
  (トラッカーはComposing解除、Decisionは Ignore、注入Ctrl+MのMを印字キー扱いしない)も定義する。
- 確定キー解決(README §1.3): CommitKey と resolve_commit_key(config, ime_id) を core に実装。
  許可リスト(Windows: MS-IME / Google日本語入力、Linux: mozc系、macOS: 空)をcore内のデータとして持つ。
- テスト: §1.3 判定表の全行、確定キー解決(許可リスト一致→CtrlM / 不一致・None→Enter /
  config明示指定が最優先 / macOSリスト空)、§1.4 の全遷移、タイムアウト境界、リピート無視、
  other_mods無視、「Unknown を Yes 扱いしない」ことの明示的なテスト。モックの ImeStateProvider を使う。
- tests/ にシナリオ統合テスト: 「日本語入力→CapsLock→確定」「確定直後にCapsLock→無反応」
  「クリック後にCapsLock→無反応」(README §5.5 T1/T2/T4 に対応するロジック版)。

受け入れ基準: ハーネス全コマンド成功。core層に unsafe・OS依存コードが無いこと。
```

### M2 — Windows バックエンド

```text
マイルストーンM2を実装してください。ブランチ名: m2-windows

README §3.1 の表の通りに src/platform/windows/ を実装:
- WH_KEYBOARD_LL(スキャンコード0x3AでCapsLock識別。vkCodeではなくscanCodeで判定すること)、
  WH_MOUSE_LL、SetWinEventHook(EVENT_SYSTEM_FOREGROUND)。フックコールバックは最小処理とし、
  イベントをチャネルでワーカースレッドの Engine へ渡す。CapsLock の抑止判定のみコールバック内で同期的に行う。
- ImeStateProvider: ImmGetDefaultIMEWnd + WM_IME_CONTROL/IMC_GETOPENSTATUS を SendMessageTimeoutW で。
  タイムアウト・失敗は Unknown。ime_id は TSF ITfInputProcessorProfileMgr::GetActiveProfile で取得し、
  MS-IME / Google日本語入力の CLSID と照合する(★要検証: AGENTS.md §7。確認できなければ None に落として
  Enter 解決とし、その旨をPRに記載)。
- KeyInjector: SendInput + dwExtraInfoマーカー。Enter と Ctrl+M の一連(Ctrl down→M down→M up→Ctrl up)の
  両方を実装。フックは LLKHF_INJECTED とマーカーで自イベントを Ignore として通知。
- 観測イベント分類: Ctrl の押下状態をフック内で追跡し、物理 Ctrl+M を CommitLikeKeyDown として通知(README §1.4)。
- Autostart: HKCU Run キー(README §3.1)。install/uninstall の冪等性を保つ。
- doctor: フック設置可否、IMEウィンドウ取得可否を診断。
- Ctrl+C / SIGTERM / コンソールクローズでフック解除して終了。

あなたの環境では実行テスト不可。cargo check --target x86_64-pc-windows-msvc を必ず通し、
PRに README §5.5 T1〜T9 の Windows 実機確認依頼を明記すること。

受け入れ基準: ハーネス全コマンド成功。安全ルール§2の1,2,4,5,6を満たすことをコードコメントとPR説明で示す。
```

### M3 — macOS バックエンド

```text
マイルストーンM3を実装してください。ブランチ名: m3-macos

README §3.2 の表の通りに src/platform/macos/ を実装:
- 起動時 hidutil で CapsLock→F18 リマップ(子プロセス実行)、終了時・パニック時に必ず復元。
  復元は Drop + シグナルハンドラの両方で保証し、doctor にも復元機能を付ける。
- CGEventTap: F18 keydown を捕捉・抑止、他キー・マウスダウンは listen-only で観測。
  タップが無効化された場合(タイムアウト等)の再有効化処理を入れる。
- ImeStateProvider: TISCopyCurrentKeyboardInputSource を extern "C" で宣言し、source ID に "Japanese" を
  含むかで ime_active を判定。ime_id には source ID をそのまま渡す(macOSの許可リストは空のため
  auto では常に Enter に解決される。README §1.3)。
- KeyInjector: CGEventPost。Enter(kVK_Return=36)に加え、commit_key 明示指定用に Ctrl+M
  (kVK_ANSI_M=46 + Control フラグ)も実装。kCGEventSourceUserData マーカーで自イベント識別。
- 観測イベント分類: CGEventGetFlags の Control フラグで物理 Ctrl+M を CommitLikeKeyDown として通知(README §1.4)。
- shift_passthrough: IOHIDSetModifierLockState を試み、リンク/実行不能なら未サポートとして warn ログ+doctor 表示
  (README §3.2 の通りベストエフォート)。
- Autostart: LaunchAgent plist の生成と launchctl bootstrap/bootout。
- doctor: TCC権限(Input Monitoring/Accessibility)の状態推定と許可手順の案内、hidutil残留の検出・復元。

cargo check --target aarch64-apple-darwin を必ず通し、PRに T1〜T9 の macOS 実機確認依頼と、
TCC許可の初回手順(手動で clime run → 許可 → install-autostart)を明記すること。

受け入れ基準: ハーネス全コマンド成功。クリーンアップ保証(§2-5)のコードパスをPR説明で列挙。
```

### M4 — Linux バックエンド

```text
マイルストーンM4を実装してください。ブランチ名: m4-linux

README §3.3 の表の通りに src/platform/linux/ を実装:
- evdev 読み取り専用監視(EVIOCGRABしない)。KEY_CAPSLOCK=58。キーボード/マウスの列挙とホットプラグ追従。
- 観測イベント分類: KEY_LEFTCTRL/KEY_RIGHTCTRL の状態を追跡し、物理 Ctrl+M を CommitLikeKeyDown として通知(README §1.4)。
- uinput 仮想デバイスで KEY_ENTER および KEY_LEFTCTRL+KEY_M の一連を注入。自デバイスは監視対象から除外。
- ImeStateProvider: zbus で fcitx5 Controller1.State() を照会。★fcitx5 の State() 戻り値の意味は
  設計時点で未検証。実装時に fcitx5 のソース/ドキュメントで確認し、根拠(URL等)をPRに記載すること。
  確認できない値は Unknown に落とす。fcitx5不在時は ibus(グローバルエンジン名)、両方不在は Unknown。
  ime_id は fcitx5 Controller1.CurrentInputMethod() / ibus のエンジン名を渡す(mozc系 → Ctrl+M 解決。README §1.3)。
- XKB caps:none の自動設定: X11(setxkbmap)と GNOME(gsettings)のみ自動。他は doctor で手順案内。
  uninstall-autostart 時に自動設定分を元に戻す。
- Autostart: systemd user unit、systemd 不在なら XDG autostart にフォールバック。
- doctor: /dev/input 読み取り権限、/dev/uinput 権限、IM フレームワーク検出、XKB設定状態。不足時は
  具体的なコマンド(usermod -aG input、udevルール内容)を表示。

Linux はあなたの環境でビルド・testまで可能だが、/dev/input や DBus は無い想定。単体で切り出せる部分
(デバイス選別ロジック、doctor の判定ロジック等)はモックでユニットテストする。
PRに T1〜T9 の Linux(X11/Wayland 両方)実機確認依頼を明記すること。

受け入れ基準: ハーネス全コマンド成功。
```

### M5 — リリース整備

```text
マイルストーンM5を実装してください。ブランチ名: m5-release

- .github/workflows/release.yml: タグ v* で win x64 / mac universal(aarch64+x86_64 を lipo)/ linux x64 の
  リリースバイナリを GitHub Releases に添付(README §5.3)。
- doctor の出力を3OSで見直し、ユーザーが自力でセットアップ完了できる文面にする。
- README にインストール手順(バイナリ取得→権限設定→install-autostart)の章を追記。仕様章は変更しない。
- CHANGELOG.md を作成し v0.1.0 を記載。

受け入れ基準: ハーネス全コマンド成功。ワークフローは act 等で検証できなければ静的レビューで根拠を示す。
```

### 汎用: バグ修正・変更依頼テンプレート

```text
[共通前文]
次の問題を修正してください: <現象・再現手順・期待動作>
該当しそうな箇所: <ファイル/モジュール、不明なら省略>
制約: 安全ルール(AGENTS.md §2)を緩める修正は禁止。修正には必ず回帰テスト
(coreで再現できるならユニットテスト、できないなら手動テスト手順の追記)を伴うこと。
```

## 7. 要検証事項(実装時に一次情報で確認すること)

設計時点で未確定・要確認の項目。実装マイルストーンで遭遇したら、推測で進めず確認結果と根拠をPRに残す:

- fcitx5 `Controller1.State()` の戻り値の意味(M4)
- ibus でのエンジン名取得の正確なインターフェース(M4)
- Windows `ITfInputProcessorProfileMgr::GetActiveProfile` の呼び出し方法と、MS-IME / Google 日本語入力の CLSID 値(M2)
- MS-IME・Google 日本語入力・mozc の既定キーマップで「Ctrl+M = 確定」が成立することの実機確認(M2/M4、手動テスト T9)
- macOS 標準日本語IM・Google 日本語入力(macOS)・ATOK の Ctrl+M 既定対応(v2 の許可リスト拡充の前提)
- macOS `IOHIDSetModifierLockState` の可用性(M3、不可なら未サポートで確定)
- Windows JIS 配列での LL フック `scanCode`/`vkCode` の実値(M2、実機確認項目に含める)
- CGEventTap の listen-only と抑止を単一タップで併用する際の挙動(M3)
