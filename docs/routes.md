# 公開許可ルート表

`misskey-egress-proxy` が Misskey UDS へ転送する経路の一覧と、その根拠。
出典はすべて [misskey-dev/misskey](https://github.com/misskey-dev/misskey)
`develop` ブランチのソースを直接確認した結果（2026-09-23 時点、行番号は当時のもの。
将来 Misskey がルートを変更した場合は本表と `src/routes.rs` を合わせて
見直すこと）。

## 許可ルート

`/` と `/assets/misskey.svg` は案内ページとしてプロキシ自身が返す静的コンテンツで、
Misskey UDS へは転送しない。既定版はバイナリとコンテナイメージに同梱し、
`STATIC_DIR`（既定 `/usr/local/share/misskey-egress-proxy`）への bind mount で
差し替えられる。どちらも CSP 付きで返し（`/` は `default-src 'none'`、
SVG はそれに `sandbox` を加えたもの）、差し替えファイルが公開オリジンで
スクリプトを実行できないようにする。以下は Misskey へ転送するルートの一覧。

| パス | メソッド | 出典 | 備考 |
|---|---|---|---|
| `/.well-known/webfinger` | GET, OPTIONS | `WellKnownServerService.ts` | `meta.federation==='none'` で403（Misskey側） |
| `/.well-known/nodeinfo` | GET | 同上 | |
| `/.well-known/host-meta`, `/.well-known/host-meta.json` | GET | 同上 | legacy fallback。低コストなので含める |
| `/nodeinfo/2.0`, `/nodeinfo/2.1` | GET | `NodeinfoServerService.ts` | |
| `/inbox` | POST | `ActivityPubServerService.ts:647` | shared inbox, body 64KB上限, HTTP署名必須 |
| `/users/:user/inbox` | POST | 同上:648 | per-user inbox |
| `/notes/:note` | GET | 同上:651 | **Accept 上書き対象**（`apOrHtml` constraint） |
| `/notes/:note/activity` | GET | 同上:686 | AP専用、gate 不要 |
| `/users/:user/outbox` | GET | 同上:715 | |
| `/users/:user/followers` | GET | 同上:721 | |
| `/users/:user/following` | GET | 同上:727 | |
| `/users/:user/collections/featured` | GET | 同上:730 | pinned notes |
| `/users/:user/publickey` | GET | 同上:733 | |
| `/users/:user` | GET | 同上:763 | **Accept 上書き対象** |
| `/@:acct` | GET | 同上:781 | **Accept 上書き対象**。`acct@host` 形式もまとめて1セグメントとして一致する（`matchit` で確認済み） |
| `/emojis/:emoji` | GET | 同上:804 | 複数形のみ。単数形 `/emoji/:path` は対象外（下記） |
| `/likes/:like` | GET | 同上:826 | |
| `/follows/:follower/:followee` | GET | 同上:852 | |
| `/follows/:followRequestId` | GET | 同上:883 | |
| `/files/app-default.jpg` | GET | `FileServerService.ts:86` | 内部 Referer は内部へ 302。`MEDIA_MODE=redirect` では上流へ転送しない（下記） |
| `/files/:key`, `/files/:key/*` | GET | 同上:93,97 | drive file 本体配信。内部 Referer は内部へ 302。`MEDIA_MODE=redirect` では上流へ転送しない（下記） |
| `/proxy/:url*` | GET | 同上:106 | media-proxy。内部リダイレクト特例あり。`MEDIA_MODE=redirect` では上流へ転送せず、許可した元 URL へ 302（下記） |
| `/identicon/:x` | GET | `ServerService.ts:242`（inline route） | **要含める**。下記参照 |

## `/identicon/:x` を含める理由

`ApRendererService.renderIdenticon()`（`ApRendererService.ts:265-272`）は
`UserEntityService.getIdenticonUrl()`（`UserEntityService.ts:386-392`）が
返す `${config.url}/identicon/:username@:host` を、アバター未設定ユーザーの
actor `icon` フィールドにそのまま埋め込む
（`ApRendererService.ts:564`: `icon: avatar ? renderImage(avatar) : ... : renderIdenticon(user)`）。
つまりアバターを設定していないローカルユーザーを連合先が正しく描画するには
このパスが public である必要がある。

## 検討した上で除外したルート

`/emoji/:path(.*)`（`ServerService.ts:165`、web 表示用ショートハンド）と
`/avatar/@:acct`（`ServerService.ts:223`）は、一見 AP 連合に必要そうだが
実際には参照されていない:

- `ApRendererService.renderEmoji()`（`ApRendererService.ts:182-198`）は
  `icon.url` に `emoji.publicUrl || emoji.originalUrl` を使う。これは
  ローカルの custom emoji なら `/files/*` 相当の URL であり、
  `/emoji/:path` ではない。
- `ApRendererService.renderImage()`（`ApRendererService.ts:255-262`、
  アバター設定済みユーザーの actor `icon`）は
  `driveFileEntityService.getPublicUrl(file)` を使う。これも同様に
  `/files/*` 相当であり、`/avatar/@:acct` ではない。

どちらも Web クライアント専用のショートハンドであることが確認できたため、
federation には不要と判断し allowlist から除外した。

## 明示的に除外したルート（内部 Caddy 経由でのみ到達可能）

- `/api/*` ── ログインユーザー向け RPC。bearer/cookie 認証。
  `ApiServerService.ts` に AP 用ルートは無い。`/api/meta` や
  `/api/v1/instance/peers` も含め、すべて POST-only またはクライアント
  向けの便利エンドポイントで AP プロトコルの一部ではない。
- `/streaming` ── WebSocket、クライアント専用。
- `/oauth/*` ── クライアントの OAuth ログインフロー。
- `/healthz` ── 運用監視用。外部に晒す理由がない。
- `/.well-known/oauth-authorization-server`,
  `/.well-known/change-password` ── クライアント向け。
- `/url` ── Summaly/OGP リンクプレビュー。未認証かつ SSRF 的リスクがあり、
  Misskey 自身の `robots.txt` も disallow している。federation には不要。
- Web クライアント SPA 一式（catch-all `*`, `/manifest.json`,
  `/sw.js`, `/embed.js`, `/robots.txt`, `/opensearch.xml`,
  `/favicon.ico`, `/_info_card_`, `/bios`, `/cli`, `/flush`, `/embed/*`,
  静的アセット）── `ClientServerService.ts` で確認済み、すべてクライアント
  向け。`/` では Misskey の SPA ではなく、このプロキシ自身の案内ページだけを返す。

## Accept の上書き（3パスのみ）

`/notes/:note`, `/users/:user`, `/@:acct` は Misskey 側で HTML/AP
両対応（fastify constraint `apOrHtml`、`ActivityPubServerService.ts:594-612`）
になっているが、本プロキシは HTML を一切公開しない方針のため
（外部からの匿名アクセスには AP JSON だけ返せば十分、というのが本設計の
前提）、`src/accept_gate.rs` の `force_ap_accept` ミドルウェアで
`Accept` を `application/activity+json` に**書き換えてから**転送する。
呼び出し側の `Accept` を審査しないので、この3パスは常に AP JSON を返す。

当初は AP を明示していないリクエストを 406 で止めていたが、
`Accept: */*` や `Accept` 無しで取りに来る連合実装を巻き込んで落とすため、
上書きに改めた。どうせ書き換えるヘッダで相手を選別する意味は無い。

## `/@:user.{rss,atom,json}`（クライアント専用フィード）の拒否

`ClientServerService.ts` は `/@:user.atom`, `/@:user.rss`, `/@:user.json`
というクライアント専用のフィード経路を、`apOrHtml` constraint を付けずに
登録している。find-my-way は静的サフィックス付きパラメータを素の
パラメータより優先するため、`/@alice.rss` は `Accept` に関係なく AP 経路
`/@:acct` には到達せず、フィード（RSS/Atom/JSON）が返る。本プロキシの
`/@{acct}` は1セグメント幅でこれを丸ごと飲み込むので、
`src/feed_routes.rs` の `reject_feed_paths` ミドルウェアで
`.rss` / `.atom` / `.json` で終わる `acct` を 404 で止める。
連合には不要なクライアント機能であり、`/emoji/:path` や
`/avatar/@:acct` と同じ扱い。405 用ハンドラを先に設置してから
ルーター全体を `layer` で包むため、GET だけでなくどのメソッドでも
同じ 404 になる（`Router::method_not_allowed_fallback` はルートの
デフォルトフォールバックを置き換えるので、逆順だと `route_layer` 済みの
フォールバックごと差し替わり、`/@alice.rss` への POST だけ 405 になって
いた）。

find-my-way はルーティング前に `safeDecodeURI` + `decodeURI` で
パーセントエンコードを解決する（予約文字 `;/?:@&=+$,#` はパス構造に
ならないが、それ以外は `%2e` → `.`、`%72` → `r` のようにデコードされる）。
判定も同じ規則でデコードしてから行うため、`/@alice%2erss` も
`/@alice.rss` と同じフィードとして拒否される。

## media（`/files/*` `/proxy/*`）の扱いと `MEDIA_MODE`

`/files/*` と `/proxy/*` は `src/media_redirect.rs` の `redirect_media`
ミドルウェアが振り分ける。`Router::route_layer` でこの4つの media ルート
だけに掛けてあり、allowlist 外のパスや 404 フォールバックには掛からない
（`layer` だとフォールバックまでラップされ、`Referer` を内部に見せかけた
だけで任意の未知パスが 404 ではなく内部ホストへの 302 になってしまう）。
4ルートの定義（`get(forward)`）は両モードで同一で、`redirect` モードでも
`forward` を呼ばないだけである。GET / HEAD 以外は両モードとも従来どおり 405。

### モード × ルート × Referer

| モード | 内部 Referer あり | それ以外の `/files/*` | それ以外の `/proxy/*` |
|---|---|---|---|
| `proxy`（既定） | `INTERNAL_BASE_URL` + 自身の path?query へ 302（検証あり。不正なら 404） | Misskey UDS へそのまま転送 | Misskey UDS へそのまま転送（`url` も検証しない） |
| `redirect` | 同上 | **404**（上流へ渡さない） | `url=` が許可プレフィックスに一致すれば**元 URL へ 302**、それ以外は **404**（上流へ渡さない） |

`MEDIA_MODE` の未知値・空文字は起動時エラー。`redirect` モードでは
`INTERNAL_BASE_URL` も起動時に検証する（http/https の origin のみ。userinfo・
query・fragment・パスは不可。空白・制御文字・`\`・非 ASCII も不可なので、IDN の
ホストは punycode で書く）。検証は**書かれた文字列**に対して行い、`url` クレートが
正規化した後の姿では見ない。`Location` に出るのは書かれたままの文字列なので、
`https://h/..` や `https://h/%2e`（正規化すると path が `/` になる）、`https://h:000443`、
`https://%68`、`https://h:443`、`https://H` のように、クレートが書き換える綴りは、
`https://h` の形（小文字、既定ポートなし、先頭ゼロなし、パーセントエンコードなし、
パスなし）に直すよう示して拒否する（末尾ドットの `https://h.` は正規形なので通す）。
ポートは 1–65535 のみで、`:0` は拒否する（TLS を待ち受ける先が無い）。
内部 Referer の 302 がこの値に依存するので、
typo を実行時の 404 ではなく起動失敗にするため。`MEDIA_ALLOWED_PREFIXES` は
`proxy` モードでは検証せず無視する（設定されていれば `warn` ログ）。

拒否はすべて body を drain してから返す 404（`src/reject.rs`）。理由は debug
ログに規則名だけ出し、呼び出し側が渡した `url` の値は出さない。ログは
`RUST_LOG` で有効にする（未設定だと ERROR しか出ない）ので、上の `warn` ログも
拒否の理由も、`RUST_LOG=warn` / `RUST_LOG=debug` を設定しないと見えない。

### 内部 Referer リダイレクト（両モード共通）

`Referer` のホストが `INTERNAL_REFERER_SUFFIX`（例: `.your-tailnet.ts.net`）に
一致する場合、バイトを中継せず `INTERNAL_BASE_URL` へ 302 リダイレクトする。
帯域節約の最適化であり、セキュリティ境界ではない。`Referer` は容易に偽装できるが、
偽装して得られるのは Tailnet 宛の行き止まりの 302 だけである。
`INTERNAL_REFERER_SUFFIX` は両モードで必須で、空（空白のみを含む）は起動時エラー。
空の suffix はあらゆるホストに一致し、偽装していない通常の `Referer` まで内部扱いに
なって `INTERNAL_BASE_URL` へ 302 されるため。判定側も、suffix が空なら常に不一致とする
（起動時の拒否だけに頼らない）。

suffix は読み込み時に前後の空白を除いて小文字化する（`Referer` のホストは小文字で届くので、
大文字を含む suffix や前後に空白のある suffix は、そのままだと一致せず、内部リダイレクトが
黙って効かなくなるため）。一致判定は次のとおり。

- `.` で始まる suffix（`.your-tailnet.ts.net`、推奨の書き方）: ホストがそれで終わる
  （`ends_with`）なら内部。従来どおりで、裸のホスト `your-tailnet.ts.net` は一致しない。
- `.` で始まらない suffix（`your-tailnet.ts.net`）: ホストがそれと完全一致するか、
  `.` + suffix で終わる場合だけ内部。ラベル境界を見るので `notyour-tailnet.ts.net` は
  内部ではない。

`redirect` モードでもこの経路を残すのは、運用者自身の UI（Tailnet 経由）が
公開ドメインの `/proxy` を内部 Referer 付きで叩くため。ここを外すと本人の
リモートメディアが 404 になる。

### 内部リダイレクトの Location とパス検証（F1–F5）

Location は `INTERNAL_BASE_URL` + `req.uri().path_and_query()` で、authority と
`Host` は使わない。組み立てる前に次を検査し、1つでも落ちれば 404 にする
（`src/media_target.rs` の `internal_location`）。

| 規則 | 内容 |
|---|---|
| F1 | 全体の長さが 8192 バイト以下 |
| F2 | 全体が印字可能 ASCII（0x21–0x7E）で、生の `\` を含まない |
| F3 | path（最初の `?` の前）が `/files/` か `/proxy/` で始まる |
| F4 | path が `path_is_safe` を通る（query は不透明。こちらでデコードする層が無いので `%00` などもエンコードのまま渡り、ヘッダ注入にならない） |
| F5 | `INTERNAL_BASE_URL + path?query` を `url` クレートで再パースし、scheme / host / port が base と一致し、userinfo・fragment が無く、パスが `base の書かれたままのパス + 生のパス` とバイト一致する（WHATWG による正規化・再エンコード・`\` → `/`・dot-segment 解決が起きていない。base 自身に `/..` や `/%2e` があれば、正規化後のパスは合っても書かれたパスとは一致せず、ここで落ちる。`proxy` モードは起動時に base を検証しないので、この検査が最後の砦になる） |

`path_is_safe` は、各セグメントについて次を要求する。空でない（空セグメント `//`
と末尾 `/` を拒否）。**最大8段のパーセントデコードのどの段でも** `.` / `..`、`/`、
`\`、制御文字（0x20 未満と DEL）にならない。不正な `%`（`%zz`、末尾の `%`、
`%2`）を含まない。UTF-8 のパーセントエンコード（`%E3%81%82`）は通す。2段目
以降のデコード結果に残る `%` は打ち切るだけなので、リテラルの `%zz`
（`%25zz`）というファイル名は誤って拒否しない。判定は**書かれたままの生の
パス**に対して行う。`url` クレートの正規化が、まさに `..` を隠すため。

既に別の層で潰れているもの（追加コード不要、回帰テストのみ）: 生の CR / LF / NUL /
空白 / タブ / DEL は hyper が 400（`tests/unix_listen.rs` の W1、生の `0xff` は W2）。
先頭 `//` `/\` は `/files/` `/proxy/` の静的前置きに一致せず 404 フォールバック。
絶対形式 `GET http://evil/files/x` は authority と `Host` が無視される。`#` は
`http` が request-target から切り落とす（生の `#` を付けた `url` は、fragment
の無い URL として判定される）。`/files` `/files/` `/proxy` `/proxy/` は
どのルートにも一致せず 404。

`http 1.5.0` は path で `\` `"` `{` `}` `|` `^` と 0x80 以上を**通す**
（`tests/dependency_behavior.rs` の DB-H1〜DB-H3）。axum の照合は生パスで、
デコードも正規化もしない（DB-R1〜DB-R3）。したがって F2 / F4 / F5 が要る。

**既定モードの唯一の挙動変更**: 従来は内部 Referer 付きなら target の中身を
見ずに 302 していた。今は上の検査に落ちる target（例: `/files/../x`）は
`proxy` モードでも 404 になる。転送側（`forward`）の経路は一切触らない
（連合互換のため。パスもクエリも正規化しない。`tests/router.rs` の R12 で固定）。

### `MEDIA_MODE=redirect` の `/proxy/*?url=`

`/proxy/*` の path 部分は解釈しない。Misskey が外へ出す URL は常に
`${mediaProxy}/${mode ?? 'image'}.webp?url=…` の形で、`url` の無い
`/proxy/<host>/<path>` 形式（`'https://' + request.params.url`）はハンドラ側の
互換分岐にすぎないので、非対応（404）にしている。

このプロセスは `url` を**取得しない**。宛先へ**クライアント**を向けるだけなので、
問題になるのは SSRF ではなく、(i) オープンリダイレクト、(ii) パーサ差異
（このプロセスと、Location を辿る側とで同じ文字列のホストの読みが割れること）、
(iii) クライアント側の内部アドレス（`169.254.169.254` など）への誘導である。
許可リストの**完全一致**で (i) と (iii) は構造的に塞がり、残る (ii) を次の規則で
潰す。Location は生の `url` 文字列ではなく、**パース後に正規化して再シリアライズ
した値**（差が出るのはホストの大小文字・既定ポートの除去・先頭ゼロ・クエリの
再エンコードだけ）。

`MEDIA_ALLOWED_PREFIXES` は `https://host[:port][/path-prefix]` のカンマ区切り。
候補と同じ `url` クレートでパースして比較するので、IDN・大小文字・既定ポートの
扱いが対称になる。非 ASCII のホスト（IDN）のエントリは黙って punycode に正規化されて許可される（`allowed_prefixes_accept_an_idn_written_either_way` が固定している設計）ので、運用者は punycode で書くこと。同形異義文字のタイポは、意図したものとは別のホストを許可することになる。

| 規則 | 内容 |
|---|---|
| E1 | `Url::parse` が成功し、scheme は `https` のみ（`http://` は平文への 302 になるので拒否。スキーム無しのエントリにはエラー文で `https://` を付けるよう促す） |
| E2 | host は完全一致のドメイン名のみ。IP リテラル、数字終わりのホスト、ワイルドカード、先頭・末尾のドット、空ラベルは拒否 |
| E3 | userinfo / query / fragment は不可。ポートは 1–65535 で、`:0`（`:00` なども）は拒否する（TLS を待ち受ける先が無く、クライアントが辿れない `Location` にしかならない） |
| E4 | path は書かれたままの形で `path_is_safe` を通り、`url` に書き換えられない形であること。末尾 `/` を補う（`/bucket` → `/bucket/`）。省略はホスト全体 |
| E5 | 空要素は無視（末尾カンマ可）、重複は畳む。1つも無ければ `/proxy` は全拒否 |
| E6 | `proxy` モードでは無視 |

1つでも不正なら起動エラーで、エントリを名指しする。照合は host 文字列の完全一致・
ポート一致（省略は 443）・パスの**セグメント境界**での前置き（`/bucket/` は
`/bucketevil/` に一致しない）で、プレフィックスそのものや末尾 `/`（オブジェクト名が
空）は不可。パスの大文字小文字は区別する（S3 / R2 のキーがそうなので）。

**`/proxy/*?url=` の検証（U0–U8）**。`original_location` が順に評価し、1つでも
落ちれば 404。

| 規則 | 内容 | 潰すもの |
|---|---|---|
| F1 | 全体の長さが 8192 バイト以下 | 過大 |
| U0 | query を `form_urlencoded` で解析し、デコード後のキーが**完全一致で `url`** のものが**ちょうど 1 個**。他のパラメータは無視（Location に出さない） | 無し / 重複 / 順序違い / エンコードされたキー（`u%72l`）。Misskey は `url` が文字列でなければ 400 を返す（重複キーは Fastify の既定パーサで配列になり 400 になるはずだが、実挙動は**未確認**）。この側は「どちらの値を採るか」を決めずに拒否する保守的な選択で、ずれを作らない。`URL` は別のキーなので `url` 無し扱い |
| U1 | 値の長さが 1..=2048 | 空 `url=`、過大 |
| U2 | 値の全バイトが 0x21..=0x7E | 制御文字（`url` クレートは TAB / LF / CR を**黙って取り除く**ので、除去後のホストが別物になる）、DEL、空白、生の非 ASCII（同形異字、全角ドット `U+3002`）、`+`・`%20` のデコード結果の空白 |
| U3 | 先頭 8 文字が `https://`（大文字小文字は不問） | `http:` `ftp:` `javascript:` `data:` `file:`、スキーム無し、`https:host` `https:/host` `https:\\host` `https:///host`（`url` クレートは 1 つのホストに束ねるが、他のパーサは束ねない） |
| U4 | 値に `\` と `#` を含まない | `\` を `/` と読む WHATWG と読まない RFC 3986 系の差、fragment（`https://a#@evil/` 系）。`%23` `%5c` は U5 / U6 の側で扱う |
| U5 | authority（`https://` 直後から最初の `/` か `?` まで）が `[A-Za-z0-9.-]+(:[0-9]{1,5})?` に完全一致 | userinfo（`@` を 1 つでも含む。空 userinfo も）、ホストのパーセントエンコード、IPv6 リテラル・ゾーン ID、空ポート、非数字ポート、空ホスト |
| U6 | authority の後の**生のパス**（最初の `?` の前）が `/` で始まり（パス無しは拒否）、`path_is_safe` を通る | dot-segment（`/../` `%2e%2e` `.%2e` `%2e.`）、エンコード済み `/` `\`、`%00` `%0d%0a`、二重・三重エンコード、空セグメント、末尾 `/`、不正な `%` |
| U7 | `Url::parse(値)` が成功し、scheme = `https`、host が `Host::Domain`、userinfo・fragment が無い | 数字ホスト（`2130706433` `0x7f.1` `0177.0.0.1` `127.1`）は U5 を通るが**ここで `Host::Ipv4` になり拒否**。ポート 65536 以上 |
| U8 | 許可プレフィックスと照合（host 完全一致 / ポート一致 / パスのセグメント境界の前置きでオブジェクト名が非空）。パース後のパスが U6 の生のパスと**バイト一致**（`url` クレートが何も正規化・再エンコードしていない）。Location は `parsed.as_str()` で、再パースしても同一かつ ASCII | 許可外ホスト、接頭辞・接尾辞の罠（`evil-s3.example.com` `s3.example.com.evil.example`）、末尾ドット、ポート違い、プレフィックス外、境界の罠（`/bucketevil/`）。`url` に書き換えられるパス（`"` `{` `}` `<`）も拒否（安全側） |

### `url` クレートと RFC 3986 系パーサの差異

「検証したホスト」と「クライアントが辿るホスト」がずれる典型で、上の規則の根拠。
`tests/dependency_behavior.rs` がいずれも実測して固定している（`Cargo.lock` の
更新でこれらが落ちたら、この節の根拠を見直すこと）。

| 挙動 | 実測 |
|---|---|
| special スキーム（`https`）は `:` の後の `/` と `\` を何個でも（0個でも）読み飛ばす | DB-U1 |
| TAB / LF / CR を入力のどこからでも取り除く。先頭の空白は trim | DB-U3 |
| `\` が authority を終わらせる（`https://a.example\@evil.example/` は host = `a.example`、path = `/@evil.example/`。RFC 3986 系では host = `evil.example`） | DB-U2 |
| userinfo は**最後の `@`** で区切る。空 userinfo は黙って落とす | DB-U2 |
| host はパーセントデコードしてから IDNA にかける。数字で終わるホストは `Host::Ipv4` に落とす | DB-U4, DB-U5 |
| 末尾ドットは保持する | DB-U4 |
| port: 空は許容、先頭ゼロ許容、65535 超と非数字はエラー、既定ポートは除去 | DB-U6 |
| パスは dot-segment を解決し、`\` を `/` にする。`%2f` は保持。パス無しでも `/` が付く（だから U6 は**生のパス**で判定する） | DB-U7 |
| 再シリアライズは冪等（U8 の前提） | DB-U8 |
| `form_urlencoded`: `+` は空白、キーもデコードする、不正な UTF-8 は U+FFFD、`url` だけのキーは空値 | DB-F1 |

### 洗い出し表（拒否基準と根拠）

`tests/common/media_corpus.rs` の各行と同じ ID。`url` はデコード後の値、許可リストは
`https://s3.example.com/bucket/` `https://r2.example.net/` `https://misskey.example.com/files/`
`https://s3.example.com:9000/other/`（すべてプレースホルダ）。

| ID | 入力 | 判定 | 規則 |
|---|---|---|---|
| K1 | `url` 無し / `?url` / `?url=` | 404 | U0 / U1 |
| K2 | `url=A&url=B`、`url=B&url=A`、`url=A&url=A`、`url=A&u%72l=B` | 404 | U0 |
| K3 | `URL=…` のみ / `url[]=…` | 404 | U0 |
| K4 | `/proxy/s3.example.com/bucket/a.png`（path 形式・`url` 無し） | 404 | U0 |
| K5 | 内側の `&` が生（`url=https://s3…/a?x=1&y=2`） | 302 `…/a?x=1`（`y` は落ちる。Misskey も同じ） | — |
| K6 | 外側で `+` | 404 | U2 |
| Sch1–Sch4 | `http:` `ftp:` `javascript:` `data:` `file:`、スキーム無し、`https:/host` `https:\\host` `https:host`、先頭空白・TAB | 404 | U3 / U2（`https:///host` は U5） |
| Sch5 | `HTTPS://S3.EXAMPLE.COM/bucket/a.png` | 302（正規化） | — |
| Au1–Au4 | `evil@s3…`、`s3…@evil`、`a@b@s3…`、`s3…:pw@evil`、`@s3…` | 404 | U5 |
| Au5 | `https://s3.example.com\@evil.example/…` | 404 | U4 |
| Au6 | `s3.example.com%5c@evil…`、`%40evil…` | 404 | U5 |
| Au7 | `https://s3.example.com#@evil…` / `?@evil…` | 404 | U4 / U6 |
| Au8 | `https://r2.example.net/@evil.example/a.png`（パス内の `@`） | 302（同一 URL） | — |
| Ho1, Ho5 | `%73%33.example.com`、`s3.example.com%E3%80%82evil` | 404 | U5 |
| Ho2, Ho3 | 末尾ドット、`.s3…`、`s3..…`、接頭辞・接尾辞の罠 | 404 | U8（`../` は U6） |
| Ho4 | キリル文字の s、`U+3002` | 404 | U2 |
| IP1 | `127.0.0.1` `2130706433` `0x7f.1` `0177.0.0.1` `127.1` `169.254.169.254` | 404 | U7 |
| IP2 | `[::1]` `[::ffff:127.0.0.1]` `[fe80::1%25eth0]` | 404 | U5 |
| IP3 | `localhost` `s3` | 404 | U8 |
| Po1 | `:443` `:00443` | 302（既定ポートを除去） | — |
| Po2, Po3 | 許可外のポート（`:8443` `:80` `:0`）/ 明記した非標準ポート | 404 / 302 | U8 |
| Po4, Po5 | 空・非数字・全角のポート / 65536 | 404 | U5 / U2 / U7 |
| Fr1, Fr2 | `#frag` / `%23` | 404 / 302（エンコードのまま） | U4 |
| Ct1 | TAB / LF / CR / NUL / DEL | 404 | U2 |
| En1 | 外側の二重エンコード | 404 | U3 |
| En2 | `%2e%2e` `%2E%2E` `.%2e` `%2e.` `%2e`、`%2f` `%5c`、`%252e%252e` `%252f` `%255c` `%25252e%25252e`、`%00` `%0d%0a`、不正な `%` | 404 | U6 |
| Pa1 | プレフィックス外、`/bucketevil/`、`/bucket`、`/bucket/`、`//` | 404 | U6 / U8 |
| Pa2 | `/bucket/../other/…`、`/bucket/%2e%2e/other/…` | 404 | U6 |
| Pa3 | 自ドメインの `/proxy/…`、`/`、`/files/../proxy/x` | 404 | U8 / U6 |
| Le1 | 値 2049 バイト / 全体 8193 バイト | 404 | U1 / F1 |
| Ok1 | `…/bucket/a%20b%E3%81%82.png`、`…/bucket/dir/a.png?x=%2F..%2F`、`https://misskey.example.com/files/<uuid>`、`https://r2.example.net/a.png` | 302（同一 URL） | — |

不変条件は `tests/router.rs` の R7 が総当たりで検査する: Location は「拒否」か「許可ホストの
許可パスに正規化された URL」のいずれかで、再パースしても同一・`https`・userinfo と
fragment 無し・ASCII で、RFC 3986 風の分割と `Url::host_str()` が同じホストを返す。

### 意図的に対応しないもの

- **`static` `avatar` `emoji` `preview` `badge` などの加工**: 元 URL へ 302 するので
  Misskey 側の加工（webp 変換・縮小・静止画化）は**行われない**。すべて無視して
  原本へ飛ばす。
- **署名付き URL**: `MEDIA_ALLOWED_PREFIXES` に載せるオブジェクトは **public-read
  （認証なしで取得できる）前提**。署名付き URL（`X-Amz-Signature` 等のクエリ）は
  非対応。Location はクエリを再シリアライズするため署名が変わりうるうえ、有効期限
  付きの秘密を `url` 値に載せて 302 することにもなる。
- **`url` 値の生の `&`**: 外側の `&` で分割されて `url` が途中で切れる。Misskey 側の
  読みも同じと考えられ、正規の URL は `query({url})` がエンコードするので影響しない（K5
  で挙動を固定）。
- **共有 S3 エンドポイント**: パス形式の共有エンドポイントでは、バケット（必要なら
  プレフィックス）を必ずエントリに含める。ホスト単位で書くと、同じエンドポイント上の
  **他テナントのバケットまで**許可する（R2 の公開ドメインはバケット単位なのでホスト
  単位で足りる想定）。自ドメインは必ず `/files/` を付ける（次節）。
- **`/proxy` を経由しない `/files/*`**: `redirect` モードでは、内部 Referer を持たない
  呼び出し元は公開ドメインの `/files/*` を**この proxy からは取得できない**（404）。

## 隣接プロジェクトとの分担（著者の構成）

以下は著者の運用構成の説明で、実装の分岐を変えるものではない（コード・テストは
`MEDIA_ALLOWED_PREFIXES` の値に依存しない）。確認できていない前提は「著者の構成」と
して書き、断定しない。

`/files/*` は、著者の構成ではエッジ（cloudflared の `^/files`）で
[nananek/misskey-files-proxy](https://github.com/nananek/misskey-files-proxy)（以下 mfp）
に流れる。mfp は移行済みのファイルを Cloudflare R2 へ 302 し（宛先は
`publicBaseUrl` + `keyPrefix` + accessKey）、未移行のものは `upstream`（Misskey 本体）へ
流す。`/proxy/*` を含むそれ以外はこの proxy が受け、`redirect` モードでは許可した
元 URL（自前の S3 / R2 と、自ドメインの `/files/`）へ 302 する。

```
/files/*  ─ cloudflared ─▶ mfp ─▶ { R2 へ 302 | ローカル配信 | upstream = Misskey }
それ以外  ─ cloudflared ─▶ このproxy ─▶ { 許可した元 URL へ 302 | 404 | 内部 Referer は内部へ 302 }
```

`/proxy?url=<自ドメイン>/files/<key>` は元 URL（自ドメインの `/files/<key>`）へ 302 し、
その先はエッジが mfp に渡す。この proxy に戻る二段目は起きない。そのため許可
プレフィックスに自ドメインの `https://<公開ホスト>/files/` を入れる（必ず `/files/` を
付ける）。自ドメインを許可から外す案（許可ホストの列挙と食い違う）や、内部ホストへ
直接 302 する案（Location の形が増える）は採らなかった。内部 Referer なしの
`/files/*` が 404 なのは、届くのがエッジの設定ミスのときだけで、404 なら即座に顕在化し、
tailnet の FQDN も出ないため。Misskey の `/files/:key` は accessKey であって S3 のキーとは
別物なので、公開 CDN の URL へ写像して 302 することはできない。

### 結合点と、崩れたときの挙動

**2 つのプロジェクトは密結合**である。

1. エッジのルーティング（cloudflared の `^/files` → mfp）が前提。外れると、この proxy の
   `/files/*` は 404 になる。
2. 許可プレフィックスに入れる自ドメインの `/files/` は、その先で mfp が R2 へ 302 する
   ことを前提にしている。
3. 互いを呼び出す実行時の経路は無い（mfp の `upstream` は Misskey 本体で、この proxy
   ではない）。したがって mfp が Misskey に流す `/files/:key`（未移行 / 連合ファイル /
   Misskey 自身のオブジェクトストレージ分）は、この proxy のモードとは無関係に公開
   エッジから Misskey に届く。このモードでは制御できない。
4. mfp は自分の Misskey 専用の実装で、汎用の部品ではない。

内部 Referer の流れ（Tailnet 経由の UI が公開ドメインの `/proxy` を内部 Referer 付きで
叩く）も、著者の構成についての前提である。
