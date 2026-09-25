// Shared currency formatter so the status strip (App.svelte) and the Insights
// panel format amounts identically (#208). USD-only today.
export function fmtMoney(n) {
  return new Intl.NumberFormat(undefined, {
    style: "currency",
    currency: "USD",
    maximumFractionDigits: 0,
  }).format(n ?? 0);
}

/**
 * Format an amount in its own currency. Rows with no currency (Nels-logged
 * transactions, or a budget whose transactions carry none) fall back to USD.
 * Robust to an unknown/invalid ISO code — falls back to USD rather than
 * throwing, so a bad provider code can never blank a row.
 *
 * Shared by the transactions list (#403) and the categories view (#426).
 * @param {number} amount unsigned magnitude
 * @param {string|null|undefined} currency ISO 4217 code, or null
 * @returns {string}
 */
export function formatAmount(amount, currency) {
  const code = (currency || "USD").toString().toUpperCase();
  const value = amount ?? 0;
  try {
    return new Intl.NumberFormat(undefined, {
      style: "currency",
      currency: code,
    }).format(value);
  } catch {
    return new Intl.NumberFormat(undefined, {
      style: "currency",
      currency: "USD",
    }).format(value);
  }
}
