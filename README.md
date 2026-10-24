# timetree-scraper-rust

A Rust port of [timetree-scraper](https://github.com/xuanruiqi/timetree-scraper). Scrape events from a public [TimeTree](https://timetreeapp.com/) calendar and save them as JSON, as an iCalendar (`.ics`) file you can import into Google Calendar, Apple Calendar, Outlook, and others, or as a CSV file in [Google Calendar's import format](https://support.google.com/calendar/answer/37118). It can also sync the events straight into one of your Google Calendars.

[timetree-scraper](https://github.com/xuanruiqi/timetree-scraper) の Rust 版です。[TimeTree](https://timetreeapp.com/) の公開カレンダーから予定を取得し、JSON、iCalendar（`.ics`）、または [Google カレンダーの読み込み形式](https://support.google.com/calendar/answer/37118)に沿った CSV で保存します。ICS ファイルは Google カレンダー、Apple カレンダー、Outlook などに読み込めます。また、取得した予定を Google カレンダーに直接同期することもできます。

The scraper opens the calendar in headless Chrome with [chromiumoxide](https://crates.io/crates/chromiumoxide), captures the `public_events` API responses the page makes, and clicks "Next month" repeatedly until a month comes back with no events. ICS files are written with [icalendar](https://crates.io/crates/icalendar).

[chromiumoxide](https://crates.io/crates/chromiumoxide) でヘッドレス Chrome を起動してカレンダーを開き、ページが送信する `public_events` API のレスポンスを取得します。予定が 0 件の月が返ってくるまで「Next month」（翌月）ボタンをクリックし続けます。ICS の出力には [icalendar](https://crates.io/crates/icalendar) を使用しています。

## Requirements / 動作環境

- Rust 1.85+ (edition 2024)
- Google Chrome or Chromium. It is found automatically in the usual install locations. Otherwise, set the `CHROME` environment variable to its path.
- Google Chrome または Chromium。通常のインストール先から自動で検出されます。見つからない場合は環境変数 `CHROME` にパスを指定してください。

## Installation / インストール

```sh
cargo install --path .
```

## Usage / 使い方

```sh
extract-timetree <url> [output_file] [--format <json|ics|csv>] [--timezone <zone>] [--gcal <calendar_name>]

# e.g. / 例
extract-timetree https://timetreeapp.com/public_calendars/example events.ics
extract-timetree https://timetreeapp.com/public_calendars/example events.json
extract-timetree https://timetreeapp.com/public_calendars/example events.csv --timezone Asia/Tokyo
extract-timetree https://timetreeapp.com/public_calendars/example --gcal "TimeTree events"

# without installing / インストールせずに実行
cargo run --release -- https://timetreeapp.com/public_calendars/example events.ics
```

| Argument / 引数 | Description | 説明 |
| --- | --- | --- |
| `url` | URL of the public TimeTree calendar, e.g. `https://timetreeapp.com/public_calendars/<calendar_id>` | TimeTree 公開カレンダーの URL |
| `output_file` | Path to write the results to. May be omitted when `--gcal` is given | 出力先のファイルパス。`--gcal` を指定した場合は省略可 |
| `--format` | `json`, `ics` or `csv`. If omitted, `.ics` files get ICS, `.csv` files get CSV, and anything else gets JSON | `json`、`ics`、`csv` のいずれか。省略時は拡張子が `.ics` なら ICS、`.csv` なら CSV、それ以外は JSON |
| `--timezone` | Time zone for CSV times, as an IANA name such as `Asia/Tokyo`. Defaults to the system time zone. Ignored for JSON and ICS | CSV の日時に使うタイムゾーン（`Asia/Tokyo` などの IANA 名）。省略時はシステムのタイムゾーン。JSON・ICS では使用しません |
| `--gcal` | Name of a Google Calendar to sync the events into (see [Google Calendar sync](#google-calendar-sync--google-カレンダーへの同期)). Can be combined with `output_file` | 予定を同期する Google カレンダーの名前（[Google カレンダーへの同期](#google-calendar-sync--google-カレンダーへの同期)を参照）。`output_file` と同時に指定できます |

## Output / 出力形式

### JSON

The raw event objects returned by TimeTree, unchanged. An event returned for two neighbouring months is kept only once.

TimeTree が返した予定オブジェクトをそのまま保存します。隣り合う月の両方で返された予定は 1 件にまとめます。

```json
{
  "public_events": [
    { "id": "...", "title": "...", "start_at": 1790640000000, "end_at": 1790640000000, ... }
  ]
}
```

### ICS

| ICS | TimeTree fields / フィールド |
| --- | --- |
| Title / タイトル | `title`, `name`, `summary` |
| Start, end / 開始・終了日時 | `start_at`, `end_at`, and similar / など |
| All-day / 終日 | `all_day`, and similar / など, or a date-only start / または日付のみの開始日 |
| Description / 説明 | `description`, `note`, `memo` |
| Location / 場所 | `location`, `place`, `venue` |
| UID | `timetree-<id>` |

Timed events with no end are given a one-hour duration. Because the UID is stable, re-importing the file updates existing entries instead of duplicating them. Events with no start time are skipped, and a message is printed for each one.

終了日時がない時刻指定の予定は 1 時間の予定として扱います。UID が固定されているため、同じファイルを再度読み込んでも予定は重複せず、既存の予定が更新されます。開始日時のない予定はスキップされ、その旨がメッセージで表示されます。

### CSV

A CSV file for [Google Calendar's import](https://support.google.com/calendar/answer/37118). Events are read the same way as for ICS, with the same defaults, and events with no start time are skipped in the same way.

[Google カレンダーの読み込み](https://support.google.com/calendar/answer/37118)用の CSV ファイルです。予定の各項目は ICS と同じ方法・同じ既定値で読み取り、開始日時のない予定も同様にスキップします。

| Column / 列 | Value / 値 |
| --- | --- |
| `Subject` | Title / タイトル |
| `Start Date`, `End Date` | `MM/DD/YYYY`, e.g. / 例 `03/01/2026` |
| `Start Time`, `End Time` | `H:MM AM/PM`, e.g. / 例 `7:00 PM`. Empty for all-day events / 終日の予定では空欄 |
| `All Day Event` | `True` or / または `False` |
| `Description` | Description / 説明 |
| `Location` | Location / 場所 |

- CSV has no time zone field. Times are written in the `--timezone` zone (the system time zone by default), and Google Calendar reads them in the time zone of the calendar you import into, so use the same zone for both.
- The `End Date` of an all-day event is its last day. In ICS it is the day after.
- CSV has no UID, so importing the same file twice creates duplicate events. Use ICS if you want to re-import.

- CSV にはタイムゾーンの項目がありません。日時は `--timezone` のタイムゾーン（省略時はシステムのタイムゾーン）で書き出され、Google カレンダーは読み込み先カレンダーのタイムゾーンとして解釈します。両者を同じタイムゾーンにしてください。
- 終日の予定の `End Date` は最終日です（ICS では最終日の翌日）。
- CSV には UID がないため、同じファイルを 2 回読み込むと予定が重複します。再読み込みする場合は ICS を使ってください。

## Google Calendar sync / Google カレンダーへの同期

`--gcal <calendar_name>` writes the events directly into the Google Calendar with that name, using the [Google Calendar API](https://developers.google.com/workspace/calendar/api/guides/overview).

`--gcal <calendar_name>` を指定すると、[Google Calendar API](https://developers.google.com/workspace/calendar/api/guides/overview) を使って、その名前の Google カレンダーに予定を直接書き込みます。

### Setup / 準備

The tool needs an OAuth client of your own. You only have to create it once:

1. In the [Google Cloud console](https://console.cloud.google.com/), create a project and [enable the Google Calendar API](https://console.cloud.google.com/apis/library/calendar-json.googleapis.com) for it.
2. Configure the [OAuth consent screen](https://console.cloud.google.com/auth/overview). Choose "External", and add your Google account as a test user.
3. [Create an OAuth client ID](https://console.cloud.google.com/auth/clients) with the application type "Desktop app".
4. Set the environment variables `TIMETREE_GOOGLE_CLIENT_ID` and `TIMETREE_GOOGLE_CLIENT_SECRET` to the client's ID and secret whenever you use `--gcal`. They are not saved anywhere, and the tool stops with a message naming any that are unset.

```sh
export TIMETREE_GOOGLE_CLIENT_ID=1234567890-abc.apps.googleusercontent.com
export TIMETREE_GOOGLE_CLIENT_SECRET=GOCSPX-...
```

OAuth クライアントを自分で用意する必要があります（最初の 1 回のみ）。

1. [Google Cloud コンソール](https://console.cloud.google.com/)でプロジェクトを作成し、[Google Calendar API を有効](https://console.cloud.google.com/apis/library/calendar-json.googleapis.com)にします。
2. [OAuth 同意画面](https://console.cloud.google.com/auth/overview)を設定します。「外部」を選び、自分の Google アカウントをテストユーザーに追加します。
3. アプリケーションの種類を「デスクトップ アプリ」として [OAuth クライアント ID を作成](https://console.cloud.google.com/auth/clients)します。
4. `--gcal` を使うときは、環境変数 `TIMETREE_GOOGLE_CLIENT_ID` と `TIMETREE_GOOGLE_CLIENT_SECRET` にクライアント ID とクライアント シークレットを設定してください。これらはどこにも保存されません。未設定の変数があれば、その名前を表示して終了します。

### Signing in / ログイン

The first time you use `--gcal`, the tool opens a Google sign-in page in your browser (and prints its URL in case the browser does not open). After you allow access, the credentials are saved to `~/.config/timetree-scraper/token.json`, readable only by you, and later runs use them without asking. You are asked to sign in again only when the saved credentials can no longer be used, for example after you revoke access or if `token.json` is corrupted. While the consent screen's publishing status is "Testing", Google expires them after 7 days. Delete `token.json` to sign out.

Sign-in happens before scraping starts, so a problem with it, or with the calendar name, is reported straight away.

初めて `--gcal` を使うと、ブラウザで Google のログイン画面が開きます（開かない場合のために URL も表示します）。アクセスを許可すると、認証情報が `~/.config/timetree-scraper/token.json`（本人のみ読み取り可）に保存され、次回以降はそれが使われます。保存された認証情報が使えなくなった場合（アクセス権を取り消した場合や `token.json` が壊れた場合など）にのみ、再度ログインを求められます。OAuth 同意画面の公開ステータスが「テスト」の間は、Google によって 7 日で失効します。ログアウトするには `token.json` を削除してください。

ログインは予定の取得を始める前に行うため、ログインやカレンダー名に問題があればすぐに分かります。

### How events are synced / 同期の仕組み

- `calendar_name` is the name shown in Google Calendar's sidebar, and must be a calendar you can edit. If none matches, the names of your editable calendars are listed.
- Events are read the same way as for ICS, with the same defaults, and events with no start time are skipped in the same way.
- Each event's Google Calendar ID is derived from its TimeTree ID, so syncing again updates the events instead of duplicating them. An event deleted in Google Calendar is restored on the next sync.
- Each sync overwrites the title, times, description and location of the synced events, so edits you make to them in Google Calendar are lost.
- Events removed from TimeTree are not deleted from Google Calendar.
- Events synced this way are separate from ones imported from an ICS or CSV file, so syncing into a calendar you already imported into creates duplicates.

- `calendar_name` は Google カレンダーのサイドバーに表示される名前で、編集権限のあるカレンダーである必要があります。一致するものがない場合は、編集できるカレンダーの名前を一覧表示します。
- 予定の各項目は ICS と同じ方法・同じ既定値で読み取り、開始日時のない予定も同様にスキップします。
- Google カレンダー上の予定 ID は TimeTree の ID から決まるため、再度同期しても予定は重複せず、既存の予定が更新されます。Google カレンダーで削除した予定は、次回の同期で復元されます。
- 同期のたびにタイトル、日時、説明、場所が上書きされるため、同期した予定を Google カレンダーで編集しても失われます。
- TimeTree から削除された予定は、Google カレンダーからは削除されません。
- この方法で同期した予定は ICS・CSV ファイルから読み込んだ予定とは別扱いのため、読み込み済みのカレンダーに同期すると予定が重複します。

## Limitations / 制限事項

- Only **public** TimeTree calendars are supported. Private calendars need a login, which this tool does not do.
- Scraping starts from the month the page opens on (normally the current month) and moves forward only, so earlier events are not collected.
- Scraping stops at the first month with no events. Events after an empty month are missed.
- The tool depends on TimeTree's web page and internal API (the "Next month" button's `aria-label` and the `public_events` endpoint), so changes on TimeTree's side can break it.

Please follow TimeTree's terms of service and respect the calendar owner's rights when using the data you collect.

- 対応しているのは TimeTree の**公開**カレンダーのみです。非公開カレンダーにはログインが必要ですが、本ツールはログインに対応していません。
- ページを開いた時点で表示される月（通常は今月）から先の月のみを取得します。それより前の予定は取得されません。
- 予定が 0 件の月に達した時点で取得を終了します。そのため、予定のない月より後の予定は取得されません。
- TimeTree のウェブページおよび内部 API（「Next month」ボタンの `aria-label` や `public_events` エンドポイント）に依存しているため、TimeTree 側の仕様変更により動作しなくなる可能性があります。

取得したデータを利用する際は、TimeTree の利用規約を守り、カレンダー所有者の権利を尊重してください。
