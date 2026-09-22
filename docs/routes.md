# 公開許可ルート表

`misskey-egress-proxy` が Misskey UDS へ転送する経路の一覧と、その根拠。
出典はすべて [misskey-dev/misskey](https://github.com/misskey-dev/misskey)
`develop` ブランチのソースを直接確認した結果（2026-09-23 時点、行番号は当時のもの。
将来 Misskey がルートを変更した場合は本表と `src/routes.rs` を合わせて
見直すこと）。

## 許可ルート

| パス | メソッド | 出典 | 備考 |
|---|---|---|---|
| `/.well-known/webfinger` | GET, OPTIONS | `WellKnownServerService.ts` | `meta.federation==='none'` で403（Misskey側） |
| `/.well-known/nodeinfo` | GET | 同上 | |
| `/.well-known/host-meta`, `/.well-known/host-meta.json` | GET | 同上 | legacy fallback。低コストなので含める |
| `/nodeinfo/2.0`, `/nodeinfo/2.1` | GET | `NodeinfoServerService.ts` | |
| `/inbox` | POST | `ActivityPubServerService.ts:647` | shared inbox, body 64KB上限, HTTP署名必須 |
| `/users/:user/inbox` | POST | 同上:648 | per-user inbox |
| `/notes/:note` | GET | 同上:651 | **Accept gate 対象**（`apOrHtml` constraint） |
| `/notes/:note/activity` | GET | 同上:686 | AP専用、gate 不要 |
| `/users/:user/outbox` | GET | 同上:715 | |
| `/users/:user/followers` | GET | 同上:721 | |
| `/users/:user/following` | GET | 同上:727 | |
| `/users/:user/collections/featured` | GET | 同上:730 | pinned notes |
| `/users/:user/publickey` | GET | 同上:733 | |
| `/users/:user` | GET | 同上:763 | **Accept gate 対象** |
| `/@:acct` | GET | 同上:781 | **Accept gate 対象**。`acct@host` 形式もまとめて1セグメントとして一致する（`matchit` で確認済み） |
| `/emojis/:emoji` | GET | 同上:804 | 複数形のみ。単数形 `/emoji/:path` は対象外（下記） |
| `/likes/:like` | GET | 同上:826 | |
| `/follows/:follower/:followee` | GET | 同上:852 | |
| `/follows/:followRequestId` | GET | 同上:883 | |
| `/files/app-default.jpg` | GET | `FileServerService.ts:86` | |
| `/files/:key`, `/files/:key/*` | GET | 同上:93,97 | drive file 本体配信 |
| `/proxy/:url*` | GET | 同上:106 | media-proxy。内部リダイレクト特例あり（下記） |
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
- Web クライアント SPA 一式（`/`, catch-all `*`, `/manifest.json`,
  `/sw.js`, `/embed.js`, `/robots.txt`, `/opensearch.xml`,
  `/favicon.ico`, `/_info_card_`, `/bios`, `/cli`, `/flush`, `/embed/*`,
  静的アセット）── `ClientServerService.ts` で確認済み、すべてクライアント
  向け。

## Accept ゲート（3パスのみ）

`/notes/:note`, `/users/:user`, `/@:acct` は Misskey 側で HTML/AP
両対応（fastify constraint `apOrHtml`、`ActivityPubServerService.ts:594-612`）
になっているが、本プロキシは HTML を一切公開しない方針のため
（外部からの匿名アクセスには AP JSON だけ返せば十分、というのが本設計の
前提）、`src/accept_gate.rs` の `require_ap_accept` ミドルウェアで
`Accept` に `application/activity+json` または `application/ld+json` を
含まないリクエストを 406 で止める。

## media の内部リダイレクト特例

`/files/*`, `/proxy/*` は `src/media_redirect.rs` の
`redirect_internal_referer` ミドルウェアで、`Referer` のホストが
`INTERNAL_REFERER_SUFFIX`（例: `.your-tailnet.ts.net`）に一致する場合、
バイトを中継せず `INTERNAL_BASE_URL` へ 302 リダイレクトする。帯域節約の
最適化であり、セキュリティ境界ではない。
