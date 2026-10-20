'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const { formatPrice } = require('./money.js');

test('two decimals', () => {
  assert.equal(formatPrice(150), '$1.50');
  assert.equal(formatPrice(5), '$0.05');
  assert.equal(formatPrice(1999), '$19.99');
});

test('zero', () => {
  assert.equal(formatPrice(0), '$0.00');
});

test('thousands separators', () => {
  assert.equal(formatPrice(123450), '$1,234.50');
});

test('refunds are negative', () => {
  assert.equal(formatPrice(-500), '-$5.00');
});

test('other currencies', () => {
  assert.equal(formatPrice(1200, 'EUR'), '€12.00');
  assert.equal(formatPrice(399, 'GBP'), '£3.99');
});

test('unknown currency', () => {
  assert.throws(() => formatPrice(100, 'XYZ'), /unknown currency/);
});
