# WinGet一覧とメタデータの調査

確認日: 2026-11-08。v0.2.0から固定のおすすめ・プリセットを廃止しました。画像や旧カタログで対象を制限せず、利用者のPCでWinGetから一覧を取得します。

## 全件取得

実行するコマンドは次のとおりです。

```powershell
winget search . --source winget --accept-source-agreements --disable-interactivity
```

対象はWinGetコミュニティソースです。Microsoft Storeの全アプリを取得する機能ではありません。`--count`を指定しないため、以前の50件制限はありません。2026-11-08の実行では16,041件が返りました。件数はソースの更新に応じて変わります。

WinGetのパッケージIDには区切りのドットがあるため、この検索で対象を列挙します。Microsoftの資料には、空の検索語とソース指定による全件取得方法も記載されています。`--count`を指定する場合の最大値は1000であるため、全件取得には使いません。[Microsoftのsearch資料](https://learn.microsoft.com/en-us/windows/package-manager/winget/search)、[IDの公式スキーマ](https://github.com/microsoft/winget-cli/blob/master/schemas/JSON/manifests/v1.12.0/manifest.defaultLocale.1.12.0.json)

一覧は名前・正確なID・提供バージョンから作成します。読み取れない形式や省略されたIDがあれば取得をエラーにし、不完全な一覧を全件取得済みとして扱いません。詳細な提供元・利用条件・ホームページは選択した項目の詳細確認時に取得します。

## カテゴリーとタグ

現在の一覧が使う`winget`コミュニティソースには、すべてのアプリで統一されたカテゴリー欄がありません。Microsoft Storeの`msstore`ソースは別であり、後述のとおりカテゴリーを取得できる例があります。「WinGet全体でカテゴリーを取得できない」という解釈は訂正します。[ソースの公式説明](https://learn.microsoft.com/en-us/windows/package-manager/winget/source)

コミュニティソースの公開マニフェストとAPIには、任意の検索用語である`Tags`があります。タグは付いていない場合もあり、綴り・粒度・言語も提供元によって異なります。[公開メタデータ](https://github.com/microsoft/winget-cli/blob/master/src/Microsoft.Management.Deployment/CatalogPackageMetadata.h)、[マニフェストのTags定義](https://github.com/microsoft/winget-cli/blob/master/schemas/JSON/manifests/v1.12.0/manifest.defaultLocale.1.12.0.json)

そのため、自動取得したアプリを推測で一般・ゲーム・クリエイター・デベロッパーに割り振りません。内部のカテゴリーは「未分類」とし、画面は「すべてのアプリ・選択中・独自登録」の表示切り替えと、WinGetのタグ検索を提供します。

タグ検索には`winget search --tag browser --source winget`等を使います。初期一覧を取得した後に利用でき、ローカルの名前検索とも組み合わせられます。旧カテゴリーは独自登録や既存プロファイルの互換性のため残しています。

### 全タグの一覧（v0.3.0）

「タグ一覧から選ぶ」で、取得できたタグをすべて表示します。タグ名の検索、選択によるWinGetタグ検索、再取得、ローカルキャッシュに対応します。数万件はListViewで表示中の行だけ生成し、件数の上限による先頭だけの表示は行いません。

取得元は現在の利用者に登録された`Microsoft.Winget.Source`の`Public/index.db`です。WinGetが取得済みのコミュニティ索引を、Windows標準の`winsqlite3.dll`で読み取り専用で開きます。アプリごとの詳細を数万回問い合わせる処理は行いません。2026-11-08、索引バージョン`2026.1107.2223.4`で23,318タグの読み取りを確認しました。

索引1系の`tags.tag`と2系の`tags2.tag`をサポートします。これらはWinGetの内部形式であり、将来の互換性は保証できません。未対応形式・索引未登録・権限制限等の場合は理由を表示し、前回の一覧とアプリ選択を保持します。手入力のタグ検索は継続して利用できます。[1系の公式実装](https://github.com/microsoft/winget-cli/blob/master/src/AppInstallerRepositoryCore/Microsoft/Schema/1_0/TagsTable.h)、[2系の公式実装](https://github.com/microsoft/winget-cli/blob/master/src/AppInstallerRepositoryCore/Microsoft/Schema/2_0/TagsTable.h)

同じ表記の重複だけを除外し、`.NET`と`.net`、日本語、数字、記号は元の表記を保持します。`- cli`のようなタグも値として渡せるよう、検索には`--tag=<タグ名>`を使います。タグの付いていないアプリは引き続き全件一覧で選択できます。タグ数はアプリ数やインストール数ではありません。対象は取得できた索引の範囲であり、Microsoft Store全体のタグを列挙するものではありません。

### Microsoft Storeのカテゴリー取得を実証

2026-11-08に、次のStore製品で取得を確認しました。アプリの導入は行っていません。

| Store製品 | Store ID | 取得したカテゴリー |
|---|---|---|
| Visual Studio Code | XP9KHM4BK9FZ7Q | Developer tools |
| Blender | 9PP3C07GTVRH | Multimedia design |
| Blender 3.6 LTS | 9PF6NVNS3F0P | Multimedia design |

例えば次のコマンドで、`Agreements`内に`Category: Developer tools`が表示されます。

```powershell
winget show --id XP9KHM4BK9FZ7Q --source msstore --accept-source-agreements --disable-interactivity
```

同じ内容がStoreソースの`packageManifests/<Store ID>?Market=JP`のJSONで、`Versions[].DefaultLocale.Agreements`の`AgreementLabel: Category`と`Agreement`に返ることも確認しました。[VS Codeの応答元](https://storeedgefd.dsx.mp.microsoft.com/v9.0/packageManifests/XP9KHM4BK9FZ7Q?Market=JP)、[Store側のカテゴリー定義](https://learn.microsoft.com/en-us/windows/apps/publish/store-submission-api)

ただし、現在のコミュニティ一覧のID（例`Microsoft.VisualStudioCode`）とStore IDは別です。同名だけで対応付けると、非公式アプリ、LTS、Insiders、異なるエディションを混同します。Appx/MSIXの`PackageFamilyName`、`ProductCode`等の一致を照合の根拠にできる場合がありますが、すべてのパッケージに共通の対応キーがあるわけではありません。[照合用フィールドの公式定義](https://github.com/microsoft/winget-cli/blob/master/schemas/JSON/manifests/v1.12.0/manifest.installer.1.12.0.json)

今後Storeから分類を補完する場合は、Store IDと製品の対応を確認できた項目だけに適用し、元のカテゴリー・取得元も保存します。従来の4分類へ変換するルールは別途必要であり、対応不明・取得失敗は未分類として扱います。インストール元をStore版へ勝手に切り替えません。v0.3.0で追加した機能は全タグ一覧であり、Storeとの自動照合・カテゴリー自動分類はまだ含みません。

## 並び替え

| 項目 | 対応 |
|---|---|
| 名前 A → Z / Z → A | 対応。大文字・小文字を区別せず、同名はIDで順序を確定 |
| ID A → Z / Z → A | 対応 |
| 選択中を先に | 対応。同じ状態の項目は名前・ID順 |
| 世界全体のインストール数 | WinGetの取得データに項目がないため未対応 |
| 評価・レビュー点数 | WinGetの取得データに項目がないため未対応 |

日本語等の名前はUnicode文字列の順序です。読み仮名による五十音順は実装していません。インストール数・評価が必要な場合は別の正当なデータ源とパッケージIDの対応が必要です。ダウンロード数やGitHubのStar数をインストール数として表示したり、架空の評価を付けたりしません。

## 保存と更新

v0.5.0では、自動出力へ含まれなかったソフトをWindowsの登録名から探す補完画面を追加しました。全件一覧の名前・IDへ空白区切りのAND / OR検索や正規表現を適用し、利用者が確認したIDだけをJSONへ追加します。登録名を空白で分割した単語も選び直せます。名前からIDを自動確定せず、通常版・EXE版・Beta等は別候補のまま提示します。[操作と制限](app-transfer.md)

取得一覧は利用者のローカル領域の`cache/winget-catalog.json`へ保存し、次回起動時に読み込みます。EXEにプリセットは含めません。取得失敗時は直前の一覧と選択内容を残します。更新時も選択中の項目・独自登録を保持します。

内部のプロファイルには選択と依存項目・独自登録を保存します。明示的な「選択中のアプリ」のJSONエクスポートは、選択と依存項目だけを書き出し、未選択の独自登録や全カタログは含めません。別PCにキャッシュがなくてもIDから復元でき、既存キャッシュがある場合は同じIDの導入方法を上書きしません。

v0.4.0では、このPCのインストール済みアプリを`winget export`で読み取り、WinGet標準JSONとして保存できます。この操作は全件検索とは別で、照合できたWinGet配信・Store配信の両方を対象とし、識別できなかったソフトの警告を表示します。インポートはプレビューから選択へ反映し、導入を直接開始しません。[操作と制限](app-transfer.md)

旧プリセットの固有IDだけが入ったプロファイルは、その選択を復元できません。新しいWinGet一覧から選び直して保存してください。独自登録と対応するWinGet検索IDのプロファイルは引き続き読み込めます。
