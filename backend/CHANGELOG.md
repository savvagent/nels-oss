# Changelog

## [0.45.1](https://github.com/savvagent/nels/compare/backend-v0.45.0...backend-v0.45.1) (2026-09-24)


### Bug Fixes

* **#562:** return bare WebAuthn options so passkey ceremonies work ([#563](https://github.com/savvagent/nels/issues/563)) ([6deb0e8](https://github.com/savvagent/nels/commit/6deb0e8486ed47a5ba862ac2ffe39e1f891e0b04)), closes [#562](https://github.com/savvagent/nels/issues/562)

## [0.45.0](https://github.com/savvagent/nels/compare/backend-v0.44.0...backend-v0.45.0) (2026-09-23)


### Features

* **#551:** Convert authentication from TOTP to WebAuthn/FIDO2 passkeys ([#552](https://github.com/savvagent/nels/issues/552)) ([844e5ea](https://github.com/savvagent/nels/commit/844e5eaece1ce18833c89ceb616d80c9312b531a))


### Bug Fixes

* **#560:** accept app.nels.money as the passkey app origin ([#561](https://github.com/savvagent/nels/issues/561)) ([3b9a660](https://github.com/savvagent/nels/commit/3b9a660a5d67f414c1dac3047f8adb6003643fbb)), closes [#560](https://github.com/savvagent/nels/issues/560)

## [0.44.0](https://github.com/savvagent/nels/compare/backend-v0.43.0...backend-v0.44.0) (2026-08-08)


### Features

* **mcp:** add native streamable http mcp server ([#540](https://github.com/savvagent/nels/issues/540)) ([6d5f136](https://github.com/savvagent/nels/commit/6d5f1369cc004e524a19d6695f70d711c7dba56f))

## [0.43.0](https://github.com/savvagent/nels/compare/backend-v0.42.1...backend-v0.43.0) (2026-08-08)


### Features

* **auth:** add RETIREMENT_PLANNER_ENABLED runtime feature flag ([#469](https://github.com/savvagent/nels/issues/469)) ([#537](https://github.com/savvagent/nels/issues/537)) ([32b7c30](https://github.com/savvagent/nels/commit/32b7c30eef41280a9727d1ce5620bb63ae332025))

## [0.42.1](https://github.com/savvagent/nels/compare/backend-v0.42.0...backend-v0.42.1) (2026-08-06)


### Bug Fixes

* **backend:** stop re-asking for retirement profile fields ([#533](https://github.com/savvagent/nels/issues/533)) ([981e184](https://github.com/savvagent/nels/commit/981e184910fd8a53df75769c6e4b10bbad9ede3b))
* **backend:** stop re-asking for retirement profile fields ([#533](https://github.com/savvagent/nels/issues/533)) ([bf95aab](https://github.com/savvagent/nels/commit/bf95aab8d9accdcab00c68265f68e8ba1c067953))

## [0.42.0](https://github.com/savvagent/nels/compare/backend-v0.41.4...backend-v0.42.0) (2026-08-06)


### Features

* **backend:** add Plaid Investments retirement asset linking ([#468](https://github.com/savvagent/nels/issues/468)) ([55f108c](https://github.com/savvagent/nels/commit/55f108c0751cc5148d32702bd39bbffcb2925c7a))
* **backend:** add Plaid Investments retirement asset linking ([#468](https://github.com/savvagent/nels/issues/468)) ([95b6cc9](https://github.com/savvagent/nels/commit/95b6cc91684f31b64872ebe1580e47c164244b7e))
* **backend:** add pure retirement projection engine ([#467](https://github.com/savvagent/nels/issues/467)) ([bf6d87a](https://github.com/savvagent/nels/commit/bf6d87a1c1cfce19335c524a806838643a8eed87))
* **backend:** add retirement projection REST handlers ([#467](https://github.com/savvagent/nels/issues/467)) ([2eca22f](https://github.com/savvagent/nels/commit/2eca22f8babb6997221c638b5a06cdf13b38e46b))
* **backend:** add RETIREMENT_PROJECTION chat action and offline route ([#467](https://github.com/savvagent/nels/issues/467)) ([f891750](https://github.com/savvagent/nels/commit/f891750453a4c79d299ad96508f50ddc7dbe9334))
* **backend:** pin MC_DEFAULT_PATHS to measured runtime and document the engine ([#467](https://github.com/savvagent/nels/issues/467)) ([c354f20](https://github.com/savvagent/nels/commit/c354f205efe65b6fa627c19bffbdd1253cfef24b))
* **backend:** retirement projection engine, REST and chat surfaces ([#467](https://github.com/savvagent/nels/issues/467)) ([2360f7c](https://github.com/savvagent/nels/commit/2360f7ca7de5f72cd60e0f6668c2bbcccf4f4036))
* retirement planner dashboard UI ([#469](https://github.com/savvagent/nels/issues/469)) ([#531](https://github.com/savvagent/nels/issues/531)) ([acb5bdf](https://github.com/savvagent/nels/commit/acb5bdf661b47d314b9105f0b6f12d878832aa55))

## [0.41.4](https://github.com/savvagent/nels/compare/backend-v0.41.3...backend-v0.41.4) (2026-07-31)


### Bug Fixes

* **backend:** 404 when delete_transaction/delete_budget match zero rows ([#493](https://github.com/savvagent/nels/issues/493)) ([#526](https://github.com/savvagent/nels/issues/526)) ([9137845](https://github.com/savvagent/nels/commit/9137845ba73c24bfb9ce604f39b2baa17b837de7))

## [0.41.3](https://github.com/savvagent/nels/compare/backend-v0.41.2...backend-v0.41.3) (2026-07-31)


### Bug Fixes

* **backend:** scope revoke_share lookup/delete by budget_id ([#492](https://github.com/savvagent/nels/issues/492)) ([5d413e1](https://github.com/savvagent/nels/commit/5d413e16cb82a427de85515c323632949751eb41))

## [0.41.2](https://github.com/savvagent/nels/compare/backend-v0.41.1...backend-v0.41.2) (2026-07-30)


### Bug Fixes

* **backend:** add #[ignore] to 8 DB-backed insights tests ([#483](https://github.com/savvagent/nels/issues/483)) ([f5deac9](https://github.com/savvagent/nels/commit/f5deac911d9bbdfe7ece0c7c8b7b81d35ae4ff23))

## [0.41.1](https://github.com/savvagent/nels/compare/backend-v0.41.0...backend-v0.41.1) (2026-07-29)


### Bug Fixes

* annotate chat carry as unavailable instead of silently 0.00 on DB error ([#477](https://github.com/savvagent/nels/issues/477)) ([#516](https://github.com/savvagent/nels/issues/516)) ([413cd51](https://github.com/savvagent/nels/commit/413cd5113c613a353137988eba91b95af46cc877))

## [0.41.0](https://github.com/savvagent/nels/compare/backend-v0.40.0...backend-v0.41.0) (2026-07-29)


### Features

* **#431:** source CategoriesViewResponse.currency from budgets.currency, add currency_is_mixed ([49db171](https://github.com/savvagent/nels/commit/49db171893c7015561811d29dbe222f0d3b04693))

## [0.40.0](https://github.com/savvagent/nels/compare/backend-v0.39.0...backend-v0.40.0) (2026-07-28)


### Features

* **backend:** add Social Security input and claiming-age adjustment ([49b39ef](https://github.com/savvagent/nels/commit/49b39efb62252028885c40cf161e195e7a178e8c)), closes [#466](https://github.com/savvagent/nels/issues/466)

## [0.39.0](https://github.com/savvagent/nels/compare/backend-v0.38.0...backend-v0.39.0) (2026-07-28)


### Features

* **backend:** add composite assets.owner_member_id FK to retirement_profiles ([#503](https://github.com/savvagent/nels/issues/503)) ([c643a15](https://github.com/savvagent/nels/commit/c643a159736d6aa7c45ca5a94a1a5f9c104cfc85)), closes [#500](https://github.com/savvagent/nels/issues/500)

## [0.38.0](https://github.com/savvagent/nels/compare/backend-v0.37.0...backend-v0.38.0) (2026-07-28)


### Features

* **backend:** add retirement profile and assumptions table ([#496](https://github.com/savvagent/nels/issues/496)) ([6d42876](https://github.com/savvagent/nels/commit/6d428763f456ad9de4cd07610f0b24e8ea697591)), closes [#465](https://github.com/savvagent/nels/issues/465)

## [0.37.0](https://github.com/savvagent/nels/compare/backend-v0.36.1...backend-v0.37.0) (2026-07-28)


### Features

* **backend:** add retirement balance sheet with user-scoped assets ([#490](https://github.com/savvagent/nels/issues/490)) ([edc5fb2](https://github.com/savvagent/nels/commit/edc5fb226e74c40ef86abfa09455ced0a38451a1)), closes [#464](https://github.com/savvagent/nels/issues/464)


### Bug Fixes

* **#434:** return 404 and warn when delete_category removes zero rows ([b6fe838](https://github.com/savvagent/nels/commit/b6fe8381a0a2f986f69dbc75c161cbf3286e1fb4))

## [0.36.1](https://github.com/savvagent/nels/compare/backend-v0.36.0...backend-v0.36.1) (2026-07-28)


### Bug Fixes

* **#432:** propagate a failed previous-period spend read instead of inventing carries ([11b0e26](https://github.com/savvagent/nels/commit/11b0e26cd1e10e4bf28de9115e36d9e4ea73dbd6)), closes [#432](https://github.com/savvagent/nels/issues/432)

## [0.36.0](https://github.com/savvagent/nels/compare/backend-v0.35.0...backend-v0.36.0) (2026-07-26)


### Features

* **billing:** gate primitives — budget-owner entitlement helpers (Plan B1) ([#437](https://github.com/savvagent/nels/issues/437)) ([392949d](https://github.com/savvagent/nels/commit/392949db8465c38b09c2c38245e21304584c1772))
* **billing:** owner Pro-gate for bank features (Plan B2) ([#441](https://github.com/savvagent/nels/issues/441)) ([d4c9640](https://github.com/savvagent/nels/commit/d4c9640ff820b0f00450607b8f3eb2014ab1c559))
* **billing:** tiered checkout price config (Plan B4a) ([#439](https://github.com/savvagent/nels/issues/439)) ([b0d5516](https://github.com/savvagent/nels/commit/b0d5516186ee6c78127c22dc9074f45def06f9a8))
* **billing:** trial on first budget (Plan B4b) ([#440](https://github.com/savvagent/nels/issues/440)) ([c49fb34](https://github.com/savvagent/nels/commit/c49fb34a29f0816aa0619fe7931b4d86f4ac6035))
* **billing:** two-tier write-gate rollout (Plan B3) ([#442](https://github.com/savvagent/nels/issues/442)) ([24f6cef](https://github.com/savvagent/nels/commit/24f6cef1bb16356f2db861ad395f0e3a0c137423))

## [0.35.0](https://github.com/savvagent/nels/compare/backend-v0.34.0...backend-v0.35.0) (2026-07-24)


### Features

* **billing:** two-tier entitlement core (Plan A) ([#435](https://github.com/savvagent/nels/issues/435)) ([32dc659](https://github.com/savvagent/nels/commit/32dc6593027112d89fd95f2623412146425b609d))

## [0.34.0](https://github.com/savvagent/nels/compare/backend-v0.33.0...backend-v0.34.0) (2026-07-24)


### Features

* **#426:** redesign the categories UX with a manageable budget-health view ([#427](https://github.com/savvagent/nels/issues/427)) ([bfb147f](https://github.com/savvagent/nels/commit/bfb147f7b509ced0cc5eac81da7f54518256fd8d)), closes [#426](https://github.com/savvagent/nels/issues/426)

## [0.33.0](https://github.com/savvagent/nels/compare/backend-v0.32.0...backend-v0.33.0) (2026-07-23)


### Features

* **#403:** transactions UX fidelity P5 — visual rebuild, bidirectional duplicate matching, detail view ([#422](https://github.com/savvagent/nels/issues/422)) ([40db2a4](https://github.com/savvagent/nels/commit/40db2a4ce61db5263ddfbb528a47e5bd66a5f50b))

## [0.32.0](https://github.com/savvagent/nels/compare/backend-v0.31.0...backend-v0.32.0) (2026-07-23)


### Features

* **#406:** non-destructive duplicate reconciliation (P3) ([#417](https://github.com/savvagent/nels/issues/417)) ([ed8c285](https://github.com/savvagent/nels/commit/ed8c2857b05bc2041e28523d13fbe46f84b76358))

## [0.31.0](https://github.com/savvagent/nels/compare/backend-v0.30.2...backend-v0.31.0) (2026-07-23)


### Features

* **#405:** Needs Review state + approve-from-list for transactions (P2) ([e16b715](https://github.com/savvagent/nels/commit/e16b715e1f4018aadf9210453446088dba5a0da6)), closes [#405](https://github.com/savvagent/nels/issues/405)

## [0.30.2](https://github.com/savvagent/nels/compare/backend-v0.30.1...backend-v0.30.2) (2026-07-23)


### Bug Fixes

* **#402:** window-scope budget-level limit check to match budget display ([#412](https://github.com/savvagent/nels/issues/412)) ([ac010c3](https://github.com/savvagent/nels/commit/ac010c324d258160610c56aa0457704e228a469d))

## [0.30.1](https://github.com/savvagent/nels/compare/backend-v0.30.0...backend-v0.30.1) (2026-07-23)


### Bug Fixes

* **#401:** window-scope category limit check to match Categories display ([#410](https://github.com/savvagent/nels/issues/410)) ([d9a04aa](https://github.com/savvagent/nels/commit/d9a04aa1be7850df73afa3686487c4af03495df8))

## [0.30.0](https://github.com/savvagent/nels/compare/backend-v0.29.4...backend-v0.30.0) (2026-07-23)


### Features

* **#403:** transactions provenance foundation (Phase 1) ([#404](https://github.com/savvagent/nels/issues/404)) ([5193844](https://github.com/savvagent/nels/commit/5193844579a16c62108c658fe764452501bc6c30))

## [0.29.4](https://github.com/savvagent/nels/compare/backend-v0.29.3...backend-v0.29.4) (2026-07-22)


### Bug Fixes

* **#396:** let a shared parent's collaborator see its rolled-up children ([e43d1d9](https://github.com/savvagent/nels/commit/e43d1d9db056aa4c156aaa35cae9ac52b4df0d7c))

## [0.29.3](https://github.com/savvagent/nels/compare/backend-v0.29.2...backend-v0.29.3) (2026-07-21)


### Bug Fixes

* **#391:** let a sharee make a shared budget active ([#392](https://github.com/savvagent/nels/issues/392)) ([26b3e66](https://github.com/savvagent/nels/commit/26b3e6617969cf02ae48d4a6ab7f5591465d55cd))

## [0.29.2](https://github.com/savvagent/nels/compare/backend-v0.29.1...backend-v0.29.2) (2026-07-11)


### Bug Fixes

* **#388:** reconcile stale limit notifications (clear + refresh) ([#389](https://github.com/savvagent/nels/issues/389)) ([c49cc56](https://github.com/savvagent/nels/commit/c49cc5675c48c9f57abe9fa2c5888fedb575fea3))

## [0.29.1](https://github.com/savvagent/nels/compare/backend-v0.29.0...backend-v0.29.1) (2026-07-11)


### Bug Fixes

* **#385:** tolerate missing thought in chat JSON and stop leaking raw model output ([#386](https://github.com/savvagent/nels/issues/386)) ([2642fa2](https://github.com/savvagent/nels/commit/2642fa2f0178c5e3a6b15fc407a0095ca8c280c8))

## [0.29.0](https://github.com/savvagent/nels/compare/backend-v0.28.0...backend-v0.29.0) (2026-07-10)


### Features

* **#376:** ask before auto-creating a category on an un-categorized transaction (+ reassign/cleanup) ([#379](https://github.com/savvagent/nels/issues/379)) ([f77d5ba](https://github.com/savvagent/nels/commit/f77d5ba6feaface31f6db4b8331cf9956a035567))

## [0.28.0](https://github.com/savvagent/nels/compare/backend-v0.27.1...backend-v0.28.0) (2026-07-10)


### Features

* **#374:** exclude transactions from budget totals (transfer / internal-payment ignore) ([#377](https://github.com/savvagent/nels/issues/377)) ([0f02532](https://github.com/savvagent/nels/commit/0f0253233f1fd0097e57a2ae6d699d0d4ab9a2c5))

## [0.27.1](https://github.com/savvagent/nels/compare/backend-v0.27.0...backend-v0.27.1) (2026-07-10)


### Bug Fixes

* **#364:** teach Nels to read auto-imported marker in RECENT TRANSACTIONS ([#369](https://github.com/savvagent/nels/issues/369)) ([bd831a5](https://github.com/savvagent/nels/commit/bd831a574dd3c134a40ad1f180b0cbef0751a275))

## [0.27.0](https://github.com/savvagent/nels/compare/backend-v0.26.0...backend-v0.27.0) (2026-07-08)


### Features

* **#359:** mark auto-imported transactions in chat read paths ([d17cbf6](https://github.com/savvagent/nels/commit/d17cbf699272b30eb2238d6b8c1317efc16f9da0))


### Bug Fixes

* **#358:** zero-based header uses income & savings+expense aggregates ([e327965](https://github.com/savvagent/nels/commit/e327965f3213a40736b3c59a5aeb3999a9b11ad3))
* **account:** revoke bank authorizations on account deletion ([#351](https://github.com/savvagent/nels/issues/351)) ([c7230f3](https://github.com/savvagent/nels/commit/c7230f347cb2ea4a7d582a781c74fa3d1cc82410))

## [0.26.0](https://github.com/savvagent/nels/compare/backend-v0.25.0...backend-v0.26.0) (2026-07-07)


### Features

* **#338:** dedicated Accounts page for synced bank accounts (Pro, $3/mo US) ([#342](https://github.com/savvagent/nels/issues/342)) ([f5ff755](https://github.com/savvagent/nels/commit/f5ff755b030485cbf17341530f5220b6ea8b8e59))

## [0.25.0](https://github.com/savvagent/nels/compare/backend-v0.24.0...backend-v0.25.0) (2026-07-07)


### Features

* **#340:** surface subscription status in the admin Users table ([#341](https://github.com/savvagent/nels/issues/341)) ([6967f2f](https://github.com/savvagent/nels/commit/6967f2f2f42ad9ff40270d6f70b9aa9ceae9c908))

## [0.24.0](https://github.com/savvagent/nels/compare/backend-v0.23.0...backend-v0.24.0) (2026-07-07)


### Features

* **#321:** link bank accounts via Plaid — Canada (Pro feature) ([#335](https://github.com/savvagent/nels/issues/335)) ([391cd0f](https://github.com/savvagent/nels/commit/391cd0fb0efe9cd246fd20ecdc304bb8ed9368b9))

## [0.23.0](https://github.com/savvagent/nels/compare/backend-v0.22.1...backend-v0.23.0) (2026-07-07)


### Features

* **#303:** link bank accounts via Stripe Financial Connections to auto-import transactions ([#328](https://github.com/savvagent/nels/issues/328)) ([632ca7e](https://github.com/savvagent/nels/commit/632ca7e66172e0e3147f63bc589c4c06d6f42664))
* **#320:** link bank accounts via GoCardless Bank Account Data (UK/FR/DE/IT/ES/DK/FI/NO) ([#330](https://github.com/savvagent/nels/issues/330)) ([acb39a9](https://github.com/savvagent/nels/commit/acb39a9e0882cbc1001def8bee61af86f766a753))
* **#322:** link bank accounts via Belvo (Mexico, Brazil) ([#332](https://github.com/savvagent/nels/issues/332)) ([2985107](https://github.com/savvagent/nels/commit/29851078c6394f94b4553459bc8d2292c5f7804b))
* **#323:** Basiq (Australia) + Akahu (New Zealand) bank linking ([#331](https://github.com/savvagent/nels/issues/331)) ([9aeb0ef](https://github.com/savvagent/nels/commit/9aeb0efeba1950fe529b77b8054ac2085a3bc03c))

## [0.22.1](https://github.com/savvagent/nels/compare/backend-v0.22.0...backend-v0.22.1) (2026-07-04)


### Bug Fixes

* **#317:** enforce rollup type/strategy compatibility on edit, not just at link time ([#325](https://github.com/savvagent/nels/issues/325)) ([cca90fa](https://github.com/savvagent/nels/commit/cca90fa78c5ce136fee5c17ad6ab7823e2857175))

## [0.22.0](https://github.com/savvagent/nels/compare/backend-v0.21.2...backend-v0.22.0) (2026-07-04)


### Features

* **#300:** allow selecting a budgeting strategy per budget ([#316](https://github.com/savvagent/nels/issues/316)) ([cdc82ac](https://github.com/savvagent/nels/commit/cdc82ac1f81c3df13bf638d3650ccbcfca8efad0))

## [0.21.2](https://github.com/savvagent/nels/compare/backend-v0.21.1...backend-v0.21.2) (2026-07-04)


### Bug Fixes

* **#297:** log malformed 2xx Gemini responses in rag.rs title/question generation ([#314](https://github.com/savvagent/nels/issues/314)) ([797711e](https://github.com/savvagent/nels/commit/797711e2ddcd8023b3475d1b0333c00a29511fc2))

## [0.21.1](https://github.com/savvagent/nels/compare/backend-v0.21.0...backend-v0.21.1) (2026-07-04)


### Bug Fixes

* **#299:** categories table headings reflect Income/Savings/Expense group semantics ([#311](https://github.com/savvagent/nels/issues/311)) ([d547e83](https://github.com/savvagent/nels/commit/d547e83c538a3973fc161ab5571c29314eee6a44))

## [0.21.0](https://github.com/savvagent/nels/compare/backend-v0.20.3...backend-v0.21.0) (2026-07-04)


### Features

* **#302:** add CATEGORY_AFFORDABILITY chat action ([#310](https://github.com/savvagent/nels/issues/310)) ([39bf378](https://github.com/savvagent/nels/commit/39bf378c84977d86dbc5d2d9b820bd25c3a6257b))


### Bug Fixes

* **#298:** rollup mirror rows resolve to the source's own aggregate limit/spend ([#305](https://github.com/savvagent/nels/issues/305)) ([6c6ccc1](https://github.com/savvagent/nels/commit/6c6ccc136c8a67c6ecdf80759ce330cc25008951))

## [0.20.3](https://github.com/savvagent/nels/compare/backend-v0.20.2...backend-v0.20.3) (2026-07-04)


### Bug Fixes

* **#301:** key categories-page navigation off the actual action, not the shared HTML table field ([#304](https://github.com/savvagent/nels/issues/304)) ([dea2ddf](https://github.com/savvagent/nels/commit/dea2ddff49b5402bc75343ecac276211dd5e0b77))

## [0.20.2](https://github.com/savvagent/nels/compare/backend-v0.20.1...backend-v0.20.2) (2026-07-04)


### Bug Fixes

* **#283:** bound gemini_narrative's reqwest client with a 20s timeout ([#293](https://github.com/savvagent/nels/issues/293)) ([b42f488](https://github.com/savvagent/nels/commit/b42f48823a38deaf2daacd48d99ed293a099ee02))

## [0.20.1](https://github.com/savvagent/nels/compare/backend-v0.20.0...backend-v0.20.1) (2026-07-04)


### Bug Fixes

* **#282:** chat category-balance answers use current-period spend, not lifetime ([#291](https://github.com/savvagent/nels/issues/291)) ([80fd6b4](https://github.com/savvagent/nels/commit/80fd6b43f96d93ebd5ced15f064cbfd65f7f4142))

## [0.20.0](https://github.com/savvagent/nels/compare/backend-v0.19.0...backend-v0.20.0) (2026-07-04)


### Features

* **#228:** fund categories — cumulative, bidirectional carry-over of unused category amounts ([#279](https://github.com/savvagent/nels/issues/279)) ([3ee534b](https://github.com/savvagent/nels/commit/3ee534b9369f4337d6a5bcdc6fbf95a744893c3f))
* **#258:** let users delete transactions ([#275](https://github.com/savvagent/nels/issues/275)) ([c8ba461](https://github.com/savvagent/nels/commit/c8ba461ee3f055ae84b9a26587896a21c69075aa))

## [0.19.0](https://github.com/savvagent/nels/compare/backend-v0.18.1...backend-v0.19.0) (2026-07-04)


### Features

* **#255:** per-viewer active-budget preference, decoupled from is_default ([#276](https://github.com/savvagent/nels/issues/276)) ([b1e11fe](https://github.com/savvagent/nels/commit/b1e11fe61055f83b08990fbe95a815a66945617e))

## [0.18.1](https://github.com/savvagent/nels/compare/backend-v0.18.0...backend-v0.18.1) (2026-07-03)


### Bug Fixes

* **#249:** bound remaining raw fetch/generateContent hang points with timeouts ([#268](https://github.com/savvagent/nels/issues/268)) ([661f0d7](https://github.com/savvagent/nels/commit/661f0d7856d19c9bf0f2e67b43bf4c44d7cf4c7d))

## [0.18.0](https://github.com/savvagent/nels/compare/backend-v0.17.0...backend-v0.18.0) (2026-07-03)


### Features

* **#241:** add budgets page — router-driven list of accessible budgets with switch-active ([#264](https://github.com/savvagent/nels/issues/264)) ([4057a81](https://github.com/savvagent/nels/commit/4057a816f78b3465c293df5f1c858e1f8b5deb4d))


### Bug Fixes

* **#257:** ask for clarification on ambiguous dictated transaction amounts ([#263](https://github.com/savvagent/nels/issues/263)) ([6eccb98](https://github.com/savvagent/nels/commit/6eccb988f77468bed4985dc0f8b3529483d69116))

## [0.17.0](https://github.com/savvagent/nels/compare/backend-v0.16.0...backend-v0.17.0) (2026-07-03)


### Features

* **#239:** group categories table by income/savings/expense type ([#252](https://github.com/savvagent/nels/issues/252)) ([3dafe0c](https://github.com/savvagent/nels/commit/3dafe0c9314b32527dd09f359502cae598b6d637))


### Bug Fixes

* **#238:** require Owner permission to set a budget's default flag ([#251](https://github.com/savvagent/nels/issues/251)) ([a00a5c0](https://github.com/savvagent/nels/commit/a00a5c088862df1a65301028fd40a01fffa6661e))

## [0.16.0](https://github.com/savvagent/nels/compare/backend-v0.15.0...backend-v0.16.0) (2026-07-03)


### Features

* **#231:** display budgets shared with user in chat (LIST_BUDGETS) ([#242](https://github.com/savvagent/nels/issues/242)) ([fca3781](https://github.com/savvagent/nels/commit/fca3781ed3371588073e17887a60c9e9cd92322a))


### Bug Fixes

* **#229:** category-scoped transaction listing (LIST_TRANSACTIONS) ([#245](https://github.com/savvagent/nels/issues/245)) ([492abd7](https://github.com/savvagent/nels/commit/492abd717d5bddd04533a656c9cdc8d300a50d0a))

## [0.15.0](https://github.com/savvagent/nels/compare/backend-v0.14.1...backend-v0.15.0) (2026-07-01)


### Features

* **#25:** Stripe subscription billing (Pro plan) ([#220](https://github.com/savvagent/nels/issues/220)) ([ff669c9](https://github.com/savvagent/nels/commit/ff669c9886ff990e6a7644433ef885b8920c3cc6))

## [0.14.1](https://github.com/savvagent/nels/compare/backend-v0.14.0...backend-v0.14.1) (2026-06-27)


### Bug Fixes

* **#213:** log DB errors in non-reminder notification helpers ([#216](https://github.com/savvagent/nels/issues/216)) ([c8ea950](https://github.com/savvagent/nels/commit/c8ea9504533d6f2b049d8834f17006f031462f7b))

## [0.14.0](https://github.com/savvagent/nels/compare/backend-v0.13.0...backend-v0.14.0) (2026-06-27)


### Features

* **#208:** budget status strip — zero-based + traditional summaries (server-persisted toggles) ([#211](https://github.com/savvagent/nels/issues/211)) ([14a2832](https://github.com/savvagent/nels/commit/14a2832d7f712f89c9d84fee61830a1c3d6107d1))


### Bug Fixes

* **#204:** log DB errors in reminder paths instead of swallowing them ([#212](https://github.com/savvagent/nels/issues/212)) ([d6316e1](https://github.com/savvagent/nels/commit/d6316e18e1193c828167457ee49bad04b74d2b6b))

## [0.13.0](https://github.com/savvagent/nels/compare/backend-v0.12.0...backend-v0.13.0) (2026-06-27)


### Features

* **#199:** editable transactions (REST + chat) ([#209](https://github.com/savvagent/nels/issues/209)) ([dd0e433](https://github.com/savvagent/nels/commit/dd0e433826a3df96cced6749b61df68d725c00f8))

## [0.12.0](https://github.com/savvagent/nels/compare/backend-v0.11.0...backend-v0.12.0) (2026-06-27)


### Features

* **#195:** add updated_at and searchable pgvector embedding to transactions ([#206](https://github.com/savvagent/nels/issues/206)) ([64fa577](https://github.com/savvagent/nels/commit/64fa577da93f9ce6af19e9dbefe6786a82f817db))

## [0.11.0](https://github.com/savvagent/nels/compare/backend-v0.10.0...backend-v0.11.0) (2026-06-27)


### Features

* **#198:** surface user-scoped audit rows in account data export ([#202](https://github.com/savvagent/nels/issues/202)) ([bbc3485](https://github.com/savvagent/nels/commit/bbc348557f8ab89331b647e020f5b336b6b7d86e))


### Bug Fixes

* **#201:** log DB errors in category-level limit check ([#203](https://github.com/savvagent/nels/issues/203)) ([4f5ad87](https://github.com/savvagent/nels/commit/4f5ad873c27ffcfbcf9c63b4322b1de3347339c8))

## [0.10.0](https://github.com/savvagent/nels/compare/backend-v0.9.0...backend-v0.10.0) (2026-06-27)


### Features

* **#191:** rate-limit GitHub issue filing (chat REPORT_ISSUE + /issues-create) ([#194](https://github.com/savvagent/nels/issues/194)) ([390e405](https://github.com/savvagent/nels/commit/390e4052ec4ae344194c92a9179fd3f4e37a7f94))
* **#192:** user-level audit trail for non-budget-scoped actions ([#197](https://github.com/savvagent/nels/issues/197)) ([d683f83](https://github.com/savvagent/nels/commit/d683f8391c8d167e38efe213c4ae33ff36787291))


### Bug Fixes

* **#193:** at-limit transactions are approaching not exceeded; chat no longer double-signals ([#200](https://github.com/savvagent/nels/issues/200)) ([c0cc0b1](https://github.com/savvagent/nels/commit/c0cc0b158f92769a4b9fb477601ce45fb830e4cf))

## [0.9.0](https://github.com/savvagent/nels/compare/backend-v0.8.2...backend-v0.9.0) (2026-06-26)


### Features

* wire chat REPORT_ISSUE action to real GitHub issue filing ([c20f994](https://github.com/savvagent/nels/commit/c20f9946be9b119325dccebf13d079f4c792e778))

## [0.8.2](https://github.com/savvagent/nels/compare/backend-v0.8.1...backend-v0.8.2) (2026-06-25)


### Bug Fixes

* **#185:** drop Type column, whole-dollar currency, smaller font in categories chat table ([#186](https://github.com/savvagent/nels/issues/186)) ([0fb19a8](https://github.com/savvagent/nels/commit/0fb19a8af33f35c180a0578abea1dffb08fed8e8))

## [0.8.1](https://github.com/savvagent/nels/compare/backend-v0.8.0...backend-v0.8.1) (2026-06-25)


### Bug Fixes

* **#181:** make categories table responsive (vertical-only scrollbar) ([#182](https://github.com/savvagent/nels/issues/182)) ([9378869](https://github.com/savvagent/nels/commit/9378869b178ecf6d7b3bf59ff9d67c0a7f23505d))

## [0.8.0](https://github.com/savvagent/nels/compare/backend-v0.7.3...backend-v0.8.0) (2026-06-24)


### Features

* **#174:** show users their LLM token usage (Settings, /tokens, chat) ([#177](https://github.com/savvagent/nels/issues/177)) ([5ca1c91](https://github.com/savvagent/nels/commit/5ca1c91f09f68b371b08035cc953f770c0637f00))
* **#176:** categories, limits, spending & totals as an HTML table in chat ([#180](https://github.com/savvagent/nels/issues/180)) ([f90c7e4](https://github.com/savvagent/nels/commit/f90c7e463b3a57057f1ae8ccb156719d33896dba))

## [0.7.3](https://github.com/savvagent/nels/compare/backend-v0.7.2...backend-v0.7.3) (2026-06-24)


### Bug Fixes

* **#172:** track Gemini thinking tokens in llm_usage ([dc86342](https://github.com/savvagent/nels/commit/dc863422ddaf8ab93084dd61d75ca486479c6fd9))

## [0.7.2](https://github.com/savvagent/nels/compare/backend-v0.7.1...backend-v0.7.2) (2026-06-24)


### Bug Fixes

* **#168:** classify ensure_not_closed errors, log audit failures, validate chat category types ([#170](https://github.com/savvagent/nels/issues/170)) ([0243df6](https://github.com/savvagent/nels/commit/0243df6dce49c7d6c15c548c1614be43349ff250))

## [0.7.1](https://github.com/savvagent/nels/compare/backend-v0.7.0...backend-v0.7.1) (2026-06-24)


### Bug Fixes

* **#166:** create all categories named in one chat message ([#167](https://github.com/savvagent/nels/issues/167)) ([4cb5818](https://github.com/savvagent/nels/commit/4cb5818f460875b8538d479d8a36b02a6dbc7d98))

## [0.7.0](https://github.com/savvagent/nels/compare/backend-v0.6.1...backend-v0.7.0) (2026-06-23)


### Features

* **#157:** launch insights dialog when user asks for budget insights in chat ([#158](https://github.com/savvagent/nels/issues/158)) ([c55ce4c](https://github.com/savvagent/nels/commit/c55ce4caea393fcdacdbf99b4fc14601265873d2))

## [0.6.1](https://github.com/savvagent/nels/compare/backend-v0.6.0...backend-v0.6.1) (2026-06-22)


### Performance Improvements

* **#76:** batch the hourly audit_log/notification purge deletes ([#152](https://github.com/savvagent/nels/issues/152)) ([2abf293](https://github.com/savvagent/nels/commit/2abf293b43041d087e71ae4e70108c6e025a31f3))

## [0.6.0](https://github.com/savvagent/nels/compare/backend-v0.5.2...backend-v0.6.0) (2026-06-21)


### Features

* **#140:** Admin PWA — user accounts, token usage, and time-on-system ([#144](https://github.com/savvagent/nels/issues/144)) ([edb5c31](https://github.com/savvagent/nels/commit/edb5c315856248616421eb499ee3cb4cd75e349e))

## [0.5.2](https://github.com/savvagent/nels/compare/backend-v0.5.1...backend-v0.5.2) (2026-06-20)


### Bug Fixes

* **#83:** surface DB errors and reject self-share in chat SHARE_BUDGET arm ([#141](https://github.com/savvagent/nels/issues/141)) ([e665713](https://github.com/savvagent/nels/commit/e665713111510e208fbade6639d6a78bc2338dcd))

## [0.5.1](https://github.com/savvagent/nels/compare/backend-v0.5.0...backend-v0.5.1) (2026-06-20)


### Bug Fixes

* **#132:** apply category limit on UPDATE_CATEGORY, drop spurious rename notice ([#134](https://github.com/savvagent/nels/issues/134)) ([8f46214](https://github.com/savvagent/nels/commit/8f46214b432e7bcb6bb5d4ffc26fdcf19c8b2619))

## [0.5.0](https://github.com/savvagent/nels/compare/backend-v0.4.0...backend-v0.5.0) (2026-06-20)


### Features

* **chat:** offer to set new category's limit to transaction amount on auto-create ([#123](https://github.com/savvagent/nels/issues/123)) ([#130](https://github.com/savvagent/nels/issues/130)) ([ca8fae9](https://github.com/savvagent/nels/commit/ca8fae99c372deb013cbbfd5fdc79f33802cee9d))

## [0.4.0](https://github.com/savvagent/nels/compare/backend-v0.3.0...backend-v0.4.0) (2026-06-20)


### Features

* **#116:** per-budget amount mode — fixed vs derived-from-categories ([#119](https://github.com/savvagent/nels/issues/119)) ([df93a33](https://github.com/savvagent/nels/commit/df93a33891f07b35d79e213814f79d1757d58260))

## [0.3.0](https://github.com/savvagent/nels/compare/backend-v0.2.0...backend-v0.3.0) (2026-06-20)


### Features

* **#52:** rework budget rollup to a live linked-category model ([#117](https://github.com/savvagent/nels/issues/117)) ([5c84214](https://github.com/savvagent/nels/commit/5c842140167d1d725a1936839d4dc573a3a09d9e)), closes [#52](https://github.com/savvagent/nels/issues/52)

## [0.2.0](https://github.com/savvagent/nels/compare/backend-v0.1.0...backend-v0.2.0) (2026-06-19)


### Features

* **rag:** add UPDATE_CATEGORY chat action to rename a category ([#105](https://github.com/savvagent/nels/issues/105)) ([#109](https://github.com/savvagent/nels/issues/109)) ([7a91b2a](https://github.com/savvagent/nels/commit/7a91b2a4102f4880796a6264091ef69b03690fc8))
