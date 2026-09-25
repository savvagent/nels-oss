import { describe, it, expect } from "vitest";
import en from "./i18n/locales/en.json";
import es from "./i18n/locales/es.json";
import fr from "./i18n/locales/fr.json";
import de from "./i18n/locales/de.json";
// Imported as `itLocale` (not `it`) to avoid shadowing vitest's `it()` test fn.
import itLocale from "./i18n/locales/it.json";
import pt from "./i18n/locales/pt.json";

const locales = { en, es, fr, de, it: itLocale, pt };

/** All top-level namespaces, derived from en (the source of truth). */
const ALL_NAMESPACES = Object.keys(en);

// The absent-not-zero copy guard. Deliberately NOT anchored on a leading
// currency symbol: de/es/fr/it/pt postfix the symbol, so a prefix-only pattern
// could never fire on the locales a native English author was never going to
// get wrong. Matches a standalone zero token in any position instead.
// Declared at module scope because both the #466 and #469 blocks assert on it.
const QUOTES_A_ZERO = /(^|[\s:—-])[$€£]?\s*0([.,]0+)?(\s*(?:[$€£]|USD|EUR))?($|[\s.,;!?])/;

// ---------------------------------------------------------------------------
// Full key-set parity — every locale must have the exact same set of namespace
// keys as en.  This is the SELF-MAINTAINING guard the hardcoded 6-namespace
// list (#482) could not be: adding a namespace to en.json makes it covered
// automatically, and deleting a key from en.json updates the expected set
// everywhere without editing this file.
// ---------------------------------------------------------------------------

describe("i18n comprehensive key-set parity (#482)", () => {
  it("every locale defines every namespace en does", () => {
    for (const [code, msgs] of Object.entries(locales)) {
      if (code === "en") continue;
      for (const ns of ALL_NAMESPACES) {
        expect(msgs[ns], `${code} missing namespace "${ns}"`).toBeTypeOf("object");
      }
    }
  });

  it("no locale has extra namespaces beyond en", () => {
    const enNs = new Set(ALL_NAMESPACES);
    for (const [code, msgs] of Object.entries(locales)) {
      if (code === "en") continue;
      for (const ns of Object.keys(msgs)) {
        expect(enNs.has(ns), `${code} has unexpected namespace "${ns}"`).toBe(true);
      }
    }
  });

  it("every locale's key set matches en per namespace", () => {
    for (const ns of ALL_NAMESPACES) {
      const enKeys = Object.keys(en[ns]).sort();
      for (const [code, msgs] of Object.entries(locales)) {
        if (code === "en") continue;
        const localeKeys = Object.keys(msgs[ns] ?? {}).sort();
        expect(localeKeys, `${code}.${ns} key set differs from en`).toEqual(enKeys);
      }
    }
  });
});

// ---------------------------------------------------------------------------
// Interpolation-token parity.  svelte-i18n interpolates by name, so a
// translation that drops or renames a `{token}` renders a literal placeholder
// or a blank number.  Key-set parity cannot catch this.
// ---------------------------------------------------------------------------

/** The set of `{token}` names an ICU-ish message interpolates. */
const tokensIn = (value) => new Set(String(value).match(/\{(\w+)\}/g) ?? []);

describe("i18n interpolation-token parity", () => {
  it("every locale interpolates exactly the tokens en does, per key", () => {
    for (const ns of ALL_NAMESPACES) {
      for (const [key, enValue] of Object.entries(en[ns])) {
        if (typeof enValue !== "string") continue;
        const want = [...tokensIn(enValue)].sort();
        for (const [code, msgs] of Object.entries(locales)) {
          if (code === "en") continue;
          const value = msgs[ns]?.[key];
          expect(typeof value, `${code}.${ns}.${key} is not a string`).toBe("string");
          expect([...tokensIn(value)].sort(), `${code}.${ns}.${key} token set differs from en`).toEqual(want);
        }
      }
    }
  });

  it("no locale value is an empty string", () => {
    for (const ns of ALL_NAMESPACES) {
      for (const [code, msgs] of Object.entries(locales)) {
        if (code === "en") continue;
        for (const [key, value] of Object.entries(msgs[ns] ?? {})) {
          if (typeof value !== "string") continue;
          expect(value.trim(), `${code}.${ns}.${key} is blank`).not.toBe("");
        }
      }
    }
  });

  // Guards the guard: if a refactor ever stops the extractor matching, the two
  // tests above would pass vacuously on every locale.
  it("actually finds the placeholders it is meant to compare", () => {
    expect([...tokensIn(en.categories.spentOf)].sort()).toEqual(["{limit}", "{spent}"]);
    expect([...tokensIn(en.categories.meterLabel)].sort()).toEqual(["{limit}", "{name}", "{spent}"]);
    expect([...tokensIn(en.categories.title)]).toEqual([]);
    // #466's adjustment note is the highest-stakes interpolated string in the
    // file: it is the ONE place both the entered and the adjusted Social
    // Security figures appear together. A translator who drops {entered} ships a
    // locale showing only the adjusted number — precisely what #466's compliance
    // section forbids, because the user did not produce that number, will not
    // recognise it, and cannot check it against their statement.
    expect([...tokensIn(en.retirement.ssAdjustmentNote)].sort()).toEqual([
      "{adjusted}",
      "{claimingAge}",
      "{entered}",
      "{quotedAge}",
    ]);
  });
});

// ---------------------------------------------------------------------------
// Per-feature required-key assertions.  These are SEMANTIC guards — they
// document which specific keys each feature area depends on, beyond the
// structural key-set parity above.  A feature-area key that is dropped from en
// (e.g. a removed feature) should be removed from both the parity assertion and
// these lists; a key that is merely renamed will be caught by the parity test.
// ---------------------------------------------------------------------------

// Keys the #365 Accounts redesign requires in every locale.
const REQUIRED_LINKED_ACCOUNTS_KEYS = [
  "lastSynced", "neverSynced", "syncFailed", "activeBadge",
  "confirmDisconnectTitle", "confirmDisconnectBody", "emptyHeadline", "emptySubtext",
  "proBenefit1", "proBenefit2", "proBenefit3", "proBadge", "proPrice",
  "redirectHelper", "modalHelper", "widgetHelper", "searchCountry", "searchBank",
  "groupCount", "addAccount", "refreshing", "regionNorthAmerica", "regionEurope",
  "regionLatinAmerica", "regionOceania", "ctaConnect", "ctaContinueToBank",
  "ctaContinueGeneric", "cancel", "noBanksFound", "noCountriesFound",
  "consentExpiredBadge", "disconnectedBadge",
];

describe("i18n linkedAccounts required keys (#365)", () => {
  it("every locale contains all required #365 keys", () => {
    for (const [code, msgs] of Object.entries(locales)) {
      for (const key of REQUIRED_LINKED_ACCOUNTS_KEYS) {
        expect(msgs.linkedAccounts[key], `${code}.linkedAccounts.${key} missing`).toBeTypeOf("string");
      }
    }
  });
});

// Keys the #382 auth-screen security-reassurance block.
const REQUIRED_AUTH_SECURITY_KEYS = [
  "securityHeading", "securityPasswordless", "securityData", "securityBank",
];

describe("i18n auth security required keys (#382)", () => {
  it("every locale contains all required #382 auth security keys", () => {
    for (const [code, msgs] of Object.entries(locales)) {
      for (const key of REQUIRED_AUTH_SECURITY_KEYS) {
        expect(msgs.auth[key], `${code}.auth.${key} missing`).toBeTypeOf("string");
      }
    }
  });
});

// Keys the first-run welcome requires (#457).
// `welcomeTagline` is deliberately absent here: it no longer has a call site,
// and asserting it would pin a dead key back into all six locales.
const REQUIRED_CHAT_WELCOME_KEYS = ["welcomeNamed", "welcomeAnon"];

describe("i18n chat welcome required keys (#457)", () => {
  it("every locale defines the first-run greeting strings", () => {
    for (const [code, msgs] of Object.entries(locales)) {
      for (const key of REQUIRED_CHAT_WELCOME_KEYS) {
        expect(msgs.chat[key], `${code}.chat.${key} missing`).toBeTypeOf("string");
      }
    }
  });
});

// #403 P3: the duplicate-reconciliation strings must exist in every locale.
const REQUIRED_TRANSACTIONS_KEYS = [
  "possibleDuplicate", "mergeKeepImported", "keepBoth", "resolveError",
];

describe("i18n transactions required keys (#403)", () => {
  it("every locale contains the #403 P3 duplicate-reconciliation keys", () => {
    for (const [code, msgs] of Object.entries(locales)) {
      for (const key of REQUIRED_TRANSACTIONS_KEYS) {
        expect(msgs.transactions[key], `${code}.transactions.${key} missing`).toBeTypeOf("string");
      }
    }
  });
});

// Keys the #426 categories rebuild requires in every locale.
const REQUIRED_CATEGORIES_KEYS = [
  "title", "loading", "back", "loadError", "searchPlaceholder", "all", "overLimit",
  "nearLimit", "noLimitChip", "funds", "rollover", "groupIncome",
  "groupSavings", "groupExpense", "noLimit", "spentOf", "remaining",
  "overBy", "statusHealthy", "statusNear", "statusOver", "fundBadge",
  "fundBalance", "rolloverBadge", "carriedAmount", "rollupBadge",
  "rollupSource", "summaryTitle", "noneFound", "meterLabel",
  // Mutation path (edit / create / delete).
  "edit", "editTitle", "addCategory", "addTitle", "nameLabel", "limitLabel",
  "typeLabel", "fundLabel", "rolloverLabel", "save", "cancel", "delete",
  "confirmDeleteTitle", "confirmDeleteBody", "saving", "saveFailed",
  "nameRequired", "limitInvalid", "nameTaken", "readOnly", "closedBudget",
  "currencyMixed", "mirrorNotEditable", "emptyCta",
  // The rollover checkbox is inert without the budget-level master switch, and
  // the server cannot clear a limit once set — both need saying, in every
  // locale, rather than leaving a dead control or a silently dropped edit.
  // #433 adds the SECOND reason the same control is inert: on a fund the #228
  // balance supersedes the #49 carry, so turning the budget switch on — the fix
  // `rolloverInactive` implies — changes nothing. Different cause, different
  // hint, and both must exist everywhere or a locale shows a bare checkbox.
  "rolloverInactive", "rolloverSupersededByFund", "limitNotClearable",
  // Deterministic write failures that used to fall through to the generic
  // "please try again": the row is gone, the fund/expense rule was broken, or
  // the DELETE (not a save) failed. Plus the row-vanished notice the editor
  // shows when a resync finds nothing.
  "deleteFailed", "fundExpenseOnly", "notFound", "categoryGone",
  // Load failures. `loadError` alone was UNREACHABLE — fetchApi always builds
  // `new Error(errText || "API error")`, so the `||` fallback never fired and
  // every locale saw the raw English server body. These are what the status
  // map resolves to instead.
  "loadErrorDenied", "loadErrorNotFound", "loadErrorServer",
  "loadErrorOffline", "loadErrorMalformed",
];

describe("i18n categories required keys (#426)", () => {
  it("every locale contains all required #426 keys", () => {
    for (const [code, msgs] of Object.entries(locales)) {
      for (const key of REQUIRED_CATEGORIES_KEYS) {
        expect(msgs.categories[key], `${code}.categories.${key} missing`).toBeTypeOf("string");
      }
    }
  });
});

// Keys the #466 Social Security input requires in every locale.
//
// There is no component consuming them yet — #469 builds the planner screen —
// so this block is the ONLY thing keeping them in step until then. Without it
// the six files drift silently and #469 inherits the mess.
const REQUIRED_RETIREMENT_KEYS = [
  "socialSecurityHeading",
  "ssEnteredLabel",
  "ssEnteredAtAge",
  "ssAdjustedLabel",
  "ssAdjustmentNote",
  "ssNotEntered",
  "ssNotEnteredHelp",
  "ssSourceUserEntered",
];

describe("i18n retirement key parity (#466)", () => {
  it("every locale contains all required #466 keys", () => {
    for (const [code, msgs] of Object.entries(locales)) {
      for (const key of REQUIRED_RETIREMENT_KEYS) {
        expect(msgs.retirement[key], `${code}.retirement.${key} missing`).toBeTypeOf("string");
      }
    }
  });

  // The absent-not-zero rule, asserted on the COPY rather than only on the
  // data. #469 renders a missing Social Security figure as this labelled
  // segment; a string that reads as a zero amount would defeat the whole point.
  //
  // The zero pattern deliberately does NOT anchor on a leading currency symbol.
  // An earlier version used /[$€£]\s*0/, which only matches PREFIX currency —
  // so for de/es/fr/it/pt, all of which postfix the symbol, the guard could
  // never fire. It could only fail on the one locale a native English author
  // was never going to get wrong. This matches a standalone zero token in any
  // position instead.
  it("the not-entered label reads as ABSENT and never quotes a zero amount", () => {
    for (const [code, msgs] of Object.entries(locales)) {
      const label = msgs.retirement.ssNotEntered;
      expect(label, `${code} ssNotEntered quotes a zero amount`).not.toMatch(QUOTES_A_ZERO);
      // It must actually say something DIFFERENT from the entered-figure label,
      // or the two states are indistinguishable to a reader.
      expect(label, `${code} ssNotEntered is not distinct from ssEnteredLabel`)
        .not.toBe(msgs.retirement.ssEnteredLabel);
      expect(msgs.retirement.ssNotEnteredHelp, `${code} must point at ssa.gov`).toContain("ssa.gov/myaccount");
    }
  });

  // Guards the guard above: the zero pattern must actually match the postfix
  // and bare forms it was widened to catch, or it is decorative.
  it("the zero-amount pattern catches postfix and bare currency forms", () => {
    for (const bad of ["Social Security — $0", "Social Security — 0 €", "Sécurité sociale : 0", "Rendita 0,00"]) {
      expect(QUOTES_A_ZERO.test(bad), `should flag ${bad}`).toBe(true);
    }
    for (const good of ["Social Security — not entered", "Social Security — nicht eingegeben"]) {
      expect(QUOTES_A_ZERO.test(good), `should allow ${good}`).toBe(false);
    }
  });
});

// Keys #469's planner screen requires in every locale. The key-set parity tests
// above already force the six files to match en; this block exists to say WHICH
// keys the screen depends on, and to pin the two #454 compliance strings the
// AC requires to ship with the headline, in all six locales, in the same PR.
const REQUIRED_RETIREMENT_DASHBOARD_KEYS = [
  // Navigation / states.
  "title", "openLabel", "back", "loading", "loadError", "tryAgain",
  "proRequiredTitle", "proRequiredBody", "proRequiredCta",
  "noProfileTitle", "noProfileBody",
  "unsupportedTitle", "unsupportedBody", "registerInterest",
  "noAccountsTitle", "noAccountsBody", "connectAccounts", "connecting",
  "connectError", "classifyTitle", "classifyBody", "classifyAccount",
  "assetTypeLabel", "taxTreatmentLabel", "saveClassification",
  "classifyRequired", "classifyError",
  // The headline is a RANGE (band), the median is secondary, % of goal is a
  // fraction — #454 decision 1's "range primary, single number secondary".
  "headlineLabel", "bandRange", "medianLine", "percentLine",
  // The two #454 binding disclosure strings.
  "disclosure", "sliderDisclosure",
  // Stacked income bar (absent Social Security renders as a labelled segment).
  "stackedBarTitle", "sourceOwnSavings", "sourceEmployer",
  "sourceSocialSecurity", "sourceOtherAssets", "sourceGap", "noIncome",
  "depletionTitle", "depletionAt", "depletionNone",
  // Assumptions are visible AND editable on the same screen (#454 decision 2).
  // `assumptionInflation` is deliberately absent: the engine computes in real
  // dollars and never consumes the stored inflation rate, so the screen shows
  // only what the projection was actually computed from. `assumptionRetirementAge`
  // is absent for a different reason: the retirement-age SLIDER renders that
  // value on the same card, so the read-out row would duplicate the editor.
  // The Social Security ASSUMPTION row is present, though — the AC's "figure
  // and its source" reads as a row in the assumptions table, not only as a
  // footnote. `ssAdjustedLabel`/`ssNotEntered` are the values that row and the
  // status line render; both are already pinned by the #466 block above.
  "assetsTitle", "assetBalance", "assetAsOf",
  "assumptionsTitle", "assumptionCurrentAge", "assumptionRealReturn",
  "assumptionLifeExpectancy", "assumptionTaxRate",
  "assumptionGrossIncome", "assumptionReplacementRatio", "assumptionPreTax",
  "assumptionRoth", "assumptionSocialSecurity",
  "controlsTitle", "controlPreTax", "controlRoth", "controlRealReturn",
  "controlRetirementAge", "controlSsClaimingAge", "recomputing",
  "saveToProfile", "assumptionsSaved", "saveFailed",
];

describe("i18n retirement dashboard required keys (#469)", () => {
  it("every locale contains all required #469 keys", () => {
    for (const [code, msgs] of Object.entries(locales)) {
      for (const key of REQUIRED_RETIREMENT_DASHBOARD_KEYS) {
        expect(msgs.retirement[key], `${code}.retirement.${key} missing`).toBeTypeOf("string");
      }
    }
  });

  // The band headline must stay a PAIR in every locale. A translator who
  // rewrites it as a single figure ships a bare "% of goal" dial, which #454
  // decision 1 forbids outright.
  it("the band headline interpolates exactly the {low}–{high} pair, in every locale", () => {
    for (const [code, msgs] of Object.entries(locales)) {
      const band = msgs.retirement.bandRange;
      expect(band, `${code}.retirement.bandRange lost {low}`).toContain("{low}");
      expect(band, `${code}.retirement.bandRange lost {high}`).toContain("{high}");
      expect(band, `${code}.retirement.bandRange has only one bound`).not.toContain("{p50}");
      expect(band, `${code}.retirement.bandRange quotes a zero figure`).not.toMatch(QUOTES_A_ZERO);
    }
  });

  // The #454 disclosure copy is binding. This pins the EN source and checks the
  // translated strings stay non-empty and distinct from the slider line; the
  // full meaning-check on each locale is a translator's call, but a blank or
  // duplicated string is a machine-checkable failure.
  it("the en disclosure carries every #454 mandatory element and no recommendation", () => {
    const d = en.retirement.disclosure.toLowerCase();
    expect(d).toContain("hypothetical illustration");
    expect(d).toContain("not a prediction");
    expect(d).toContain("guarantee");
    expect(d).toContain("financial advice");
    expect(d).toContain("actual results will differ");
    expect(d).toContain("registered investment adviser");
    // It disclaims recommendation without making one. A recommendation is a
    // directive to the reader ("you should", "invest in", ...); none belongs
    // here, and the AC-4 check is exactly this list.
    for (const directive of ["you should", "you consider", "switch to", "invest in", "buy ", "sell "]) {
      expect(d, `disclosure recommends: ${directive}`).not.toContain(directive);
    }
  });

  it("every locale ships both disclosure strings, non-blank and distinct", () => {
    for (const [code, msgs] of Object.entries(locales)) {
      const disclosure = msgs.retirement.disclosure;
      const slider = msgs.retirement.sliderDisclosure;
      expect(disclosure, `${code} disclosure blank`).not.toBe("");
      expect(slider, `${code} sliderDisclosure blank`).not.toBe("");
      // Distinct strings, or the slider line's "not your actual outcome" caveat
      // has collapsed into the main disclaimer and the nuance is lost.
      expect(slider, `${code} sliderDisclosure duplicates disclosure`).not.toBe(disclosure);
    }
  });
});
