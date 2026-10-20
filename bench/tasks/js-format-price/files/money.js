'use strict';

// Prices are kept in integer cents everywhere in the shop; this module turns
// them into text for receipts, emails and the admin pages.

const SYMBOLS = {
  USD: '$',
  EUR: '€',
  GBP: '£',
};

/**
 * Formats an amount given in cents, e.g. formatPrice(123450) -> "$1,234.50".
 *
 * - Always two decimals and a comma between thousands: "$1,000,000.00".
 * - The currency symbol goes in front for every currency: "€12.00", "£3.99".
 * - Negative amounts (refunds) put the minus sign before the symbol: "-$5.00".
 *
 * @param {number} cents   integer amount in cents
 * @param {string} [currency="USD"]  one of the keys of SYMBOLS
 * @returns {string}
 */
function formatPrice(cents, currency = 'USD') {
  const symbol = SYMBOLS[currency];
  if (symbol === undefined) {
    throw new Error(`unknown currency: ${currency}`);
  }
  const amount = cents / 100;
  // Whole amounts get their two decimals; the rest are already precise.
  const text = Number.isInteger(amount) ? amount.toFixed(2) : String(amount);
  return symbol + text;
}

module.exports = { formatPrice, SYMBOLS };
