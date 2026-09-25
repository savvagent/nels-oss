# Changelog

## [1.40.0](https://github.com/savvagent/nels/compare/frontend-v1.39.1...frontend-v1.40.0) (2026-09-23)


### Features

* **#551:** Convert authentication from TOTP to WebAuthn/FIDO2 passkeys ([#552](https://github.com/savvagent/nels/issues/552)) ([844e5ea](https://github.com/savvagent/nels/commit/844e5eaece1ce18833c89ceb616d80c9312b531a))


### Bug Fixes

* **#553:** translate passkey and recovery-code UI strings ([#558](https://github.com/savvagent/nels/issues/558)) ([961857d](https://github.com/savvagent/nels/commit/961857d9d2f39e7d328250ed5d1827a8e9ce6a58)), closes [#553](https://github.com/savvagent/nels/issues/553)
* **#560:** accept app.nels.money as the passkey app origin ([#561](https://github.com/savvagent/nels/issues/561)) ([3b9a660](https://github.com/savvagent/nels/commit/3b9a660a5d67f414c1dac3047f8adb6003643fbb)), closes [#560](https://github.com/savvagent/nels/issues/560)

## [1.39.1](https://github.com/savvagent/nels/compare/frontend-v1.39.0...frontend-v1.39.1) (2026-09-02)


### Bug Fixes

* **#547:** cache-bust icon URLs so browsers re-fetch after the [#542](https://github.com/savvagent/nels/issues/542) logo swap ([078d3ec](https://github.com/savvagent/nels/commit/078d3ecab4f61c29524eb104f1770d6d6e342551))

## [1.39.0](https://github.com/savvagent/nels/compare/frontend-v1.38.0...frontend-v1.39.0) (2026-09-01)


### Features

* **#542:** swap the Nels logo for the new wizard-hat mark ([#543](https://github.com/savvagent/nels/issues/543)) ([eda44a7](https://github.com/savvagent/nels/commit/eda44a7f3c0395bbdd71aa0f641c849cfd4581e9))

## [1.38.0](https://github.com/savvagent/nels/compare/frontend-v1.37.0...frontend-v1.38.0) (2026-08-08)


### Features

* **auth:** add RETIREMENT_PLANNER_ENABLED runtime feature flag ([#469](https://github.com/savvagent/nels/issues/469)) ([#537](https://github.com/savvagent/nels/issues/537)) ([32b7c30](https://github.com/savvagent/nels/commit/32b7c30eef41280a9727d1ce5620bb63ae332025))

## [1.37.0](https://github.com/savvagent/nels/compare/frontend-v1.36.2...frontend-v1.37.0) (2026-08-06)


### Features

* retirement planner dashboard UI ([#469](https://github.com/savvagent/nels/issues/469)) ([#531](https://github.com/savvagent/nels/issues/531)) ([acb5bdf](https://github.com/savvagent/nels/commit/acb5bdf661b47d314b9105f0b6f12d878832aa55))

## [1.36.2](https://github.com/savvagent/nels/compare/frontend-v1.36.1...frontend-v1.36.2) (2026-07-31)


### Bug Fixes

* **frontend:** report an already-deleted item honestly in confirmDeletion ([#494](https://github.com/savvagent/nels/issues/494)) ([#524](https://github.com/savvagent/nels/issues/524)) ([bb4737d](https://github.com/savvagent/nels/commit/bb4737d7d72c70405301e85c2f60130600ef7fce))

## [1.36.1](https://github.com/savvagent/nels/compare/frontend-v1.36.0...frontend-v1.36.1) (2026-07-30)


### Bug Fixes

* **frontend:** derive i18n namespace list from en.json instead of hardcoding ([#482](https://github.com/savvagent/nels/issues/482)) ([ef2fa80](https://github.com/savvagent/nels/commit/ef2fa80ec76a274477f80ae2161a043eca4e37db))

## [1.36.0](https://github.com/savvagent/nels/compare/frontend-v1.35.0...frontend-v1.36.0) (2026-07-29)


### Features

* **#431:** source CategoriesViewResponse.currency from budgets.currency, add currency_is_mixed ([49db171](https://github.com/savvagent/nels/commit/49db171893c7015561811d29dbe222f0d3b04693))

## [1.35.0](https://github.com/savvagent/nels/compare/frontend-v1.34.0...frontend-v1.35.0) (2026-07-29)


### Features

* **frontend:** remove first-run chat brand chip and tagline ([#461](https://github.com/savvagent/nels/issues/461)) ([e18b5dc](https://github.com/savvagent/nels/commit/e18b5dcbad5f604bf550be0ad891be472a181c7f))

## [1.34.0](https://github.com/savvagent/nels/compare/frontend-v1.33.2...frontend-v1.34.0) (2026-07-28)


### Features

* **backend:** add Social Security input and claiming-age adjustment ([49b39ef](https://github.com/savvagent/nels/commit/49b39efb62252028885c40cf161e195e7a178e8c)), closes [#466](https://github.com/savvagent/nels/issues/466)

## [1.33.2](https://github.com/savvagent/nels/compare/frontend-v1.33.1...frontend-v1.33.2) (2026-07-28)


### Bug Fixes

* **frontend:** stop the categories page advertising carries the backend never performs ([#479](https://github.com/savvagent/nels/issues/479)) ([0b8089d](https://github.com/savvagent/nels/commit/0b8089d3f6f2f697330f206c04b854a562ac59db))

## [1.33.1](https://github.com/savvagent/nels/compare/frontend-v1.33.0...frontend-v1.33.1) (2026-07-28)


### Bug Fixes

* **frontend:** suppress the Rollover badge and chip on fund categories ([#472](https://github.com/savvagent/nels/issues/472)) ([d064c6b](https://github.com/savvagent/nels/commit/d064c6b68550d268a3a82041462ddd84bc0007b8))

## [1.33.0](https://github.com/savvagent/nels/compare/frontend-v1.32.0...frontend-v1.33.0) (2026-07-27)


### Features

* **billing:** two-tier pricing on marketing + tier-aware checkout ([#446](https://github.com/savvagent/nels/issues/446)) ([b66e7cd](https://github.com/savvagent/nels/commit/b66e7cd0ba6c763745fc5e0f31bea413dc7c23eb))

## [1.32.0](https://github.com/savvagent/nels/compare/frontend-v1.31.0...frontend-v1.32.0) (2026-07-27)


### Features

* **billing:** two-tier-aware subscription UI (Plan C) ([#443](https://github.com/savvagent/nels/issues/443)) ([71ab3f5](https://github.com/savvagent/nels/commit/71ab3f5bef4af1203fbf28b0c273d3e1738e5266))

## [1.31.0](https://github.com/savvagent/nels/compare/frontend-v1.30.0...frontend-v1.31.0) (2026-07-24)


### Features

* **#426:** redesign the categories UX with a manageable budget-health view ([#427](https://github.com/savvagent/nels/issues/427)) ([bfb147f](https://github.com/savvagent/nels/commit/bfb147f7b509ced0cc5eac81da7f54518256fd8d)), closes [#426](https://github.com/savvagent/nels/issues/426)


### Bug Fixes

* **#425:** truncate long account names in the transactions list pill on mobile ([#428](https://github.com/savvagent/nels/issues/428)) ([6ef6766](https://github.com/savvagent/nels/commit/6ef6766f297d1f61daaf931e13eb736eb9ffcc78))

## [1.30.0](https://github.com/savvagent/nels/compare/frontend-v1.29.0...frontend-v1.30.0) (2026-07-23)


### Features

* **#403:** transactions UX fidelity P5 — visual rebuild, bidirectional duplicate matching, detail view ([#422](https://github.com/savvagent/nels/issues/422)) ([40db2a4](https://github.com/savvagent/nels/commit/40db2a4ce61db5263ddfbb528a47e5bd66a5f50b))

## [1.29.0](https://github.com/savvagent/nels/compare/frontend-v1.28.0...frontend-v1.29.0) (2026-07-23)


### Features

* **#407:** transactions list P4 — grouping, running totals, search, filter chips ([#420](https://github.com/savvagent/nels/issues/420)) ([577fc84](https://github.com/savvagent/nels/commit/577fc84e3e13e05c53bfaf21118ee8b0dac854a4))

## [1.28.0](https://github.com/savvagent/nels/compare/frontend-v1.27.0...frontend-v1.28.0) (2026-07-23)


### Features

* **#406:** non-destructive duplicate reconciliation (P3) ([#417](https://github.com/savvagent/nels/issues/417)) ([ed8c285](https://github.com/savvagent/nels/commit/ed8c2857b05bc2041e28523d13fbe46f84b76358))

## [1.27.0](https://github.com/savvagent/nels/compare/frontend-v1.26.0...frontend-v1.27.0) (2026-07-23)


### Features

* **#405:** Needs Review state + approve-from-list for transactions (P2) ([e16b715](https://github.com/savvagent/nels/commit/e16b715e1f4018aadf9210453446088dba5a0da6)), closes [#405](https://github.com/savvagent/nels/issues/405)

## [1.26.0](https://github.com/savvagent/nels/compare/frontend-v1.25.2...frontend-v1.26.0) (2026-07-23)


### Features

* **#403:** transactions provenance foundation (Phase 1) ([#404](https://github.com/savvagent/nels/issues/404)) ([5193844](https://github.com/savvagent/nels/commit/5193844579a16c62108c658fe764452501bc6c30))

## [1.25.2](https://github.com/savvagent/nels/compare/frontend-v1.25.1...frontend-v1.25.2) (2026-07-22)


### Bug Fixes

* **#395:** make header budget name bolder and more visible ([#397](https://github.com/savvagent/nels/issues/397)) ([29187e4](https://github.com/savvagent/nels/commit/29187e47bf27b7464176376ed096b61384c3bc27))

## [1.25.1](https://github.com/savvagent/nels/compare/frontend-v1.25.0...frontend-v1.25.1) (2026-07-21)


### Bug Fixes

* **#391:** let a sharee make a shared budget active ([#392](https://github.com/savvagent/nels/issues/392)) ([26b3e66](https://github.com/savvagent/nels/commit/26b3e6617969cf02ae48d4a6ab7f5591465d55cd))

## [1.25.0](https://github.com/savvagent/nels/compare/frontend-v1.24.0...frontend-v1.25.0) (2026-07-11)


### Features

* **#382:** UI polish & trust — auth copy, chat alignment, self-hosted font, security reassurance ([#383](https://github.com/savvagent/nels/issues/383)) ([ec06e2d](https://github.com/savvagent/nels/commit/ec06e2df5303780702b76f82c94653dbc0311b7f))

## [1.24.0](https://github.com/savvagent/nels/compare/frontend-v1.23.0...frontend-v1.24.0) (2026-07-10)


### Features

* **#376:** ask before auto-creating a category on an un-categorized transaction (+ reassign/cleanup) ([#379](https://github.com/savvagent/nels/issues/379)) ([f77d5ba](https://github.com/savvagent/nels/commit/f77d5ba6feaface31f6db4b8331cf9956a035567))

## [1.23.0](https://github.com/savvagent/nels/compare/frontend-v1.22.0...frontend-v1.23.0) (2026-07-10)


### Features

* **#372:** icon-only account card actions, drop redundant institution name ([#373](https://github.com/savvagent/nels/issues/373)) ([ec8b5c9](https://github.com/savvagent/nels/commit/ec8b5c9047f6162fb4de2657e24c47dea4ae5547))

## [1.22.0](https://github.com/savvagent/nels/compare/frontend-v1.21.2...frontend-v1.22.0) (2026-07-10)


### Features

* **#365:** Accounts page UX redesign — user-friendly linked-accounts UI ([#371](https://github.com/savvagent/nels/issues/371)) ([be9abc7](https://github.com/savvagent/nels/commit/be9abc7d29c54a4cda0c2e353b3b9f1dda4d145a))


### Bug Fixes

* **#366:** treat bodiless 2xx (202/empty) responses as no-content in fetchApi ([#367](https://github.com/savvagent/nels/issues/367)) ([750a3c9](https://github.com/savvagent/nels/commit/750a3c91dc37f5a225bdcd14f591df6891e39d45))

## [1.21.2](https://github.com/savvagent/nels/compare/frontend-v1.21.1...frontend-v1.21.2) (2026-07-10)


### Bug Fixes

* **#358:** zero-based header uses income & savings+expense aggregates ([e327965](https://github.com/savvagent/nels/commit/e327965f3213a40736b3c59a5aeb3999a9b11ad3))

## [1.21.1](https://github.com/savvagent/nels/compare/frontend-v1.21.0...frontend-v1.21.1) (2026-07-08)


### Bug Fixes

* **#303:** inject VITE_STRIPE_PUBLISHABLE_KEY into frontend build ([d4e71e7](https://github.com/savvagent/nels/commit/d4e71e778288a6f6d13180bae762f6c067614e5f))
* **#352:** add standard mobile-web-app-capable meta tag ([#353](https://github.com/savvagent/nels/issues/353)) ([f0b85c6](https://github.com/savvagent/nels/commit/f0b85c6d6f19747bb50ba3accc1e484ec277044e)), closes [#352](https://github.com/savvagent/nels/issues/352)

## [1.21.0](https://github.com/savvagent/nels/compare/frontend-v1.20.0...frontend-v1.21.0) (2026-07-07)


### Features

* **#338:** dedicated Accounts page for synced bank accounts (Pro, $3/mo US) ([#342](https://github.com/savvagent/nels/issues/342)) ([f5ff755](https://github.com/savvagent/nels/commit/f5ff755b030485cbf17341530f5220b6ea8b8e59))

## [1.20.0](https://github.com/savvagent/nels/compare/frontend-v1.19.0...frontend-v1.20.0) (2026-07-07)


### Features

* **#321:** link bank accounts via Plaid — Canada (Pro feature) ([#335](https://github.com/savvagent/nels/issues/335)) ([391cd0f](https://github.com/savvagent/nels/commit/391cd0fb0efe9cd246fd20ecdc304bb8ed9368b9))

## [1.19.0](https://github.com/savvagent/nels/compare/frontend-v1.18.1...frontend-v1.19.0) (2026-07-07)


### Features

* **#303:** link bank accounts via Stripe Financial Connections to auto-import transactions ([#328](https://github.com/savvagent/nels/issues/328)) ([632ca7e](https://github.com/savvagent/nels/commit/632ca7e66172e0e3147f63bc589c4c06d6f42664))
* **#320:** link bank accounts via GoCardless Bank Account Data (UK/FR/DE/IT/ES/DK/FI/NO) ([#330](https://github.com/savvagent/nels/issues/330)) ([acb39a9](https://github.com/savvagent/nels/commit/acb39a9e0882cbc1001def8bee61af86f766a753))
* **#322:** link bank accounts via Belvo (Mexico, Brazil) ([#332](https://github.com/savvagent/nels/issues/332)) ([2985107](https://github.com/savvagent/nels/commit/29851078c6394f94b4553459bc8d2292c5f7804b))
* **#323:** Basiq (Australia) + Akahu (New Zealand) bank linking ([#331](https://github.com/savvagent/nels/issues/331)) ([9aeb0ef](https://github.com/savvagent/nels/commit/9aeb0efeba1950fe529b77b8054ac2085a3bc03c))

## [1.18.1](https://github.com/savvagent/nels/compare/frontend-v1.18.0...frontend-v1.18.1) (2026-07-04)


### Bug Fixes

* **#317:** enforce rollup type/strategy compatibility on edit, not just at link time ([#325](https://github.com/savvagent/nels/issues/325)) ([cca90fa](https://github.com/savvagent/nels/commit/cca90fa78c5ce136fee5c17ad6ab7823e2857175))

## [1.18.0](https://github.com/savvagent/nels/compare/frontend-v1.17.2...frontend-v1.18.0) (2026-07-04)


### Features

* **#300:** allow selecting a budgeting strategy per budget ([#316](https://github.com/savvagent/nels/issues/316)) ([cdc82ac](https://github.com/savvagent/nels/commit/cdc82ac1f81c3df13bf638d3650ccbcfca8efad0))

## [1.17.2](https://github.com/savvagent/nels/compare/frontend-v1.17.1...frontend-v1.17.2) (2026-07-04)


### Bug Fixes

* **#299:** categories table headings reflect Income/Savings/Expense group semantics ([#311](https://github.com/savvagent/nels/issues/311)) ([d547e83](https://github.com/savvagent/nels/commit/d547e83c538a3973fc161ab5571c29314eee6a44))

## [1.17.1](https://github.com/savvagent/nels/compare/frontend-v1.17.0...frontend-v1.17.1) (2026-07-04)


### Bug Fixes

* **#301:** key categories-page navigation off the actual action, not the shared HTML table field ([#304](https://github.com/savvagent/nels/issues/304)) ([dea2ddf](https://github.com/savvagent/nels/commit/dea2ddff49b5402bc75343ecac276211dd5e0b77))

## [1.17.0](https://github.com/savvagent/nels/compare/frontend-v1.16.1...frontend-v1.17.0) (2026-07-04)


### Features

* **#285:** budgets list cards open budget details on click/tap/keyboard ([#289](https://github.com/savvagent/nels/issues/289)) ([d9c175c](https://github.com/savvagent/nels/commit/d9c175c673e8f7e6718c1d9f41943f4fd38d3a1f))

## [1.16.1](https://github.com/savvagent/nels/compare/frontend-v1.16.0...frontend-v1.16.1) (2026-07-04)


### Bug Fixes

* **#278:** return to chat transcript after every prompt submission ([#284](https://github.com/savvagent/nels/issues/284)) ([b03e41e](https://github.com/savvagent/nels/commit/b03e41e3fc9bb50ced4f65ded556e6a4a9960e1a))

## [1.16.0](https://github.com/savvagent/nels/compare/frontend-v1.15.0...frontend-v1.16.0) (2026-07-04)


### Features

* **#258:** let users delete transactions ([#275](https://github.com/savvagent/nels/issues/275)) ([c8ba461](https://github.com/savvagent/nels/commit/c8ba461ee3f055ae84b9a26587896a21c69075aa))
* **#261:** refactor sidebar into a MainMenu popover + History/Settings/DeleteAccount pages ([#274](https://github.com/savvagent/nels/issues/274)) ([4b96d46](https://github.com/savvagent/nels/commit/4b96d462698bc788e503df0d89ac726a37c4923f))

## [1.15.0](https://github.com/savvagent/nels/compare/frontend-v1.14.1...frontend-v1.15.0) (2026-07-03)


### Features

* **#255:** per-viewer active-budget preference, decoupled from is_default ([#276](https://github.com/savvagent/nels/issues/276)) ([b1e11fe](https://github.com/savvagent/nels/commit/b1e11fe61055f83b08990fbe95a815a66945617e))


### Bug Fixes

* **#266:** scope App.svelte's active-budget resolution to owned rows ([#272](https://github.com/savvagent/nels/issues/272)) ([e6e4d6c](https://github.com/savvagent/nels/commit/e6e4d6c77ee3c7242d9882bbcb60aa2e788a4d9a))

## [1.14.1](https://github.com/savvagent/nels/compare/frontend-v1.14.0...frontend-v1.14.1) (2026-07-03)


### Bug Fixes

* **#249:** bound remaining raw fetch/generateContent hang points with timeouts ([#268](https://github.com/savvagent/nels/issues/268)) ([661f0d7](https://github.com/savvagent/nels/commit/661f0d7856d19c9bf0f2e67b43bf4c44d7cf4c7d))

## [1.14.0](https://github.com/savvagent/nels/compare/frontend-v1.13.0...frontend-v1.14.0) (2026-07-03)


### Features

* **#241:** add budgets page — router-driven list of accessible budgets with switch-active ([#264](https://github.com/savvagent/nels/issues/264)) ([4057a81](https://github.com/savvagent/nels/commit/4057a816f78b3465c293df5f1c858e1f8b5deb4d))

## [1.13.0](https://github.com/savvagent/nels/compare/frontend-v1.12.1...frontend-v1.13.0) (2026-07-03)


### Features

* **#239:** group categories table by income/savings/expense type ([#252](https://github.com/savvagent/nels/issues/252)) ([3dafe0c](https://github.com/savvagent/nels/commit/3dafe0c9314b32527dd09f359502cae598b6d637))
* **#240:** add budget details page at /budgets/{budget_id} ([#256](https://github.com/savvagent/nels/issues/256)) ([60e307e](https://github.com/savvagent/nels/commit/60e307ef24c838f852d4346e147415b842a5039f))


### Bug Fixes

* **#238:** require Owner permission to set a budget's default flag ([#251](https://github.com/savvagent/nels/issues/251)) ([a00a5c0](https://github.com/savvagent/nels/commit/a00a5c088862df1a65301028fd40a01fffa6661e))

## [1.12.1](https://github.com/savvagent/nels/compare/frontend-v1.12.0...frontend-v1.12.1) (2026-07-03)


### Bug Fixes

* **#246:** give fetchApi an AbortController-based timeout ([#248](https://github.com/savvagent/nels/issues/248)) ([ef43b97](https://github.com/savvagent/nels/commit/ef43b974a03e1e7ad55b1f543fb1df09e4e0cbb4))

## [1.12.0](https://github.com/savvagent/nels/compare/frontend-v1.11.0...frontend-v1.12.0) (2026-07-03)


### Features

* **#230:** allow typing while Nels is responding (FIFO queue + auto-send) ([#243](https://github.com/savvagent/nels/issues/243)) ([194301b](https://github.com/savvagent/nels/commit/194301bb1bf47b02d453a900013106efa159c287))

## [1.11.0](https://github.com/savvagent/nels/compare/frontend-v1.10.0...frontend-v1.11.0) (2026-07-03)


### Features

* **#233:** router-driven main content outlet for categories/insights ([#236](https://github.com/savvagent/nels/issues/236)) ([fef9b4b](https://github.com/savvagent/nels/commit/fef9b4bc8dc9b1aa87ad678e660c3b732d82729a))

## [1.10.0](https://github.com/savvagent/nels/compare/frontend-v1.9.0...frontend-v1.10.0) (2026-07-03)


### Features

* **#232:** simplify busy chat header ([#234](https://github.com/savvagent/nels/issues/234)) ([f7ae256](https://github.com/savvagent/nels/commit/f7ae25684bacbb0d8b0b534cf2a93771cbcd4cef))

## [1.9.0](https://github.com/savvagent/nels/compare/frontend-v1.8.0...frontend-v1.9.0) (2026-07-02)


### Features

* **frontend:** lean mobile chat with windowed transcript, New Chat button, and opt-in history drawer ([a427dbf](https://github.com/savvagent/nels/commit/a427dbf9e7e12547c0f75f66b3af9e8d4fbf646b))

## [1.8.0](https://github.com/savvagent/nels/compare/frontend-v1.7.0...frontend-v1.8.0) (2026-07-01)


### Features

* **#25:** Stripe subscription billing (Pro plan) ([#220](https://github.com/savvagent/nels/issues/220)) ([ff669c9](https://github.com/savvagent/nels/commit/ff669c9886ff990e6a7644433ef885b8920c3cc6))

## [1.7.0](https://github.com/savvagent/nels/compare/frontend-v1.6.2...frontend-v1.7.0) (2026-06-27)


### Features

* **#208:** budget status strip — zero-based + traditional summaries (server-persisted toggles) ([#211](https://github.com/savvagent/nels/issues/211)) ([14a2832](https://github.com/savvagent/nels/commit/14a2832d7f712f89c9d84fee61830a1c3d6107d1))

## [1.6.2](https://github.com/savvagent/nels/compare/frontend-v1.6.1...frontend-v1.6.2) (2026-06-25)


### Bug Fixes

* **#185:** drop Type column, whole-dollar currency, smaller font in categories chat table ([#186](https://github.com/savvagent/nels/issues/186)) ([0fb19a8](https://github.com/savvagent/nels/commit/0fb19a8af33f35c180a0578abea1dffb08fed8e8))

## [1.6.1](https://github.com/savvagent/nels/compare/frontend-v1.6.0...frontend-v1.6.1) (2026-06-24)


### Bug Fixes

* **#181:** make categories table responsive (vertical-only scrollbar) ([#182](https://github.com/savvagent/nels/issues/182)) ([9378869](https://github.com/savvagent/nels/commit/9378869b178ecf6d7b3bf59ff9d67c0a7f23505d))

## [1.6.0](https://github.com/savvagent/nels/compare/frontend-v1.5.0...frontend-v1.6.0) (2026-06-24)


### Features

* **#174:** show users their LLM token usage (Settings, /tokens, chat) ([#177](https://github.com/savvagent/nels/issues/177)) ([5ca1c91](https://github.com/savvagent/nels/commit/5ca1c91f09f68b371b08035cc953f770c0637f00))
* **#176:** categories, limits, spending & totals as an HTML table in chat ([#180](https://github.com/savvagent/nels/issues/180)) ([f90c7e4](https://github.com/savvagent/nels/commit/f90c7e463b3a57057f1ae8ccb156719d33896dba))

## [1.5.0](https://github.com/savvagent/nels/compare/frontend-v1.4.0...frontend-v1.5.0) (2026-06-23)


### Features

* **#157:** launch insights dialog when user asks for budget insights in chat ([#158](https://github.com/savvagent/nels/issues/158)) ([c55ce4c](https://github.com/savvagent/nels/commit/c55ce4caea393fcdacdbf99b4fc14601265873d2))
* **#161:** add /budgets-list, /categories-list, /budgets-insights, /budgets-switch chat commands ([#165](https://github.com/savvagent/nels/issues/165)) ([b53eb9a](https://github.com/savvagent/nels/commit/b53eb9a3e8303820ad7168e46493c2457c3562a4))

## [1.4.0](https://github.com/savvagent/nels/compare/frontend-v1.3.2...frontend-v1.4.0) (2026-06-23)


### Features

* **#154:** hide mic and recall buttons on iOS and Android ([#155](https://github.com/savvagent/nels/issues/155)) ([cfecc9c](https://github.com/savvagent/nels/commit/cfecc9c5d8040e0c16a91c6f6d68b28b82c56561))

## [1.3.2](https://github.com/savvagent/nels/compare/frontend-v1.3.1...frontend-v1.3.2) (2026-06-22)


### Bug Fixes

* **#138:** clear in-progress composer text on session reset ([#149](https://github.com/savvagent/nels/issues/149)) ([01e5eaf](https://github.com/savvagent/nels/commit/01e5eaf5c5d0d21afe7c3825c33cc820a5baf1f6)), closes [#138](https://github.com/savvagent/nels/issues/138)

## [1.3.1](https://github.com/savvagent/nels/compare/frontend-v1.3.0...frontend-v1.3.1) (2026-06-21)


### Bug Fixes

* **#143:** stack recall/mic vertically with square send to widen mobile chat input ([#147](https://github.com/savvagent/nels/issues/147)) ([bdeaff3](https://github.com/savvagent/nels/commit/bdeaff3726c84a1374d24c1faa3c55ff42284447))

## [1.3.0](https://github.com/savvagent/nels/compare/frontend-v1.2.0...frontend-v1.3.0) (2026-06-20)


### Features

* **#124:** recall previous prompts via Up/Down arrows and a touch recall button ([#133](https://github.com/savvagent/nels/issues/133)) ([aa9183f](https://github.com/savvagent/nels/commit/aa9183f79821a5a629d444314e1a550fbdf165fb))


### Bug Fixes

* **#136:** clear prompt-history recall state on session reset ([#137](https://github.com/savvagent/nels/issues/137)) ([7e963d0](https://github.com/savvagent/nels/commit/7e963d0ac8b5ec46eddbf2a32c3e19b3153f45d4))

## [1.2.0](https://github.com/savvagent/nels/compare/frontend-v1.1.0...frontend-v1.2.0) (2026-06-20)


### Features

* **#126:** add Insights button to top bar left of notification icon ([#127](https://github.com/savvagent/nels/issues/127)) ([62b3a6c](https://github.com/savvagent/nels/commit/62b3a6c136b879c82c7ee6f9852f014581cf5335))


### Bug Fixes

* **#125:** installed PWA window header reads 'Nels AI Financial Agent' ([#128](https://github.com/savvagent/nels/issues/128)) ([1d939e5](https://github.com/savvagent/nels/commit/1d939e55d16b40005cdda18043e4996671ff6e82))

## [1.1.0](https://github.com/savvagent/nels/compare/frontend-v1.0.3...frontend-v1.1.0) (2026-06-20)


### Features

* **#116:** per-budget amount mode — fixed vs derived-from-categories ([#119](https://github.com/savvagent/nels/issues/119)) ([df93a33](https://github.com/savvagent/nels/commit/df93a33891f07b35d79e213814f79d1757d58260))

## [1.0.3](https://github.com/savvagent/nels/compare/frontend-v1.0.2...frontend-v1.0.3) (2026-06-19)


### Bug Fixes

* **#111:** surface the PWA update button after code-only deploys ([#114](https://github.com/savvagent/nels/issues/114)) ([b992e1e](https://github.com/savvagent/nels/commit/b992e1eba218c6458a710b0b7d64fd059982e182))

## [1.0.2](https://github.com/savvagent/nels/compare/frontend-v1.0.1...frontend-v1.0.2) (2026-06-19)


### Bug Fixes

* **#108:** reliably scroll chat to newest content ([#112](https://github.com/savvagent/nels/issues/112)) ([fc9984f](https://github.com/savvagent/nels/commit/fc9984fcdf254741c624095b19a60fdc53d9fe5b))

## [1.0.1](https://github.com/savvagent/nels/compare/frontend-v1.0.0...frontend-v1.0.1) (2026-06-19)


### Bug Fixes

* restore chat input focus after send ([511e6f1](https://github.com/savvagent/nels/commit/511e6f1de8f74fdd0c2f2e6b50b58fc9c0c28ad2)), closes [#104](https://github.com/savvagent/nels/issues/104)
