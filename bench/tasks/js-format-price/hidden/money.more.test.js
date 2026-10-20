'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const { formatPrice } = require('./money.js');

test('default currency is dollars', () => {
  assert.equal(formatPrice(250), '$2.50');
  assert.equal(formatPrice(250, 'USD'), '$2.50');
});

test('small amounts', () => {
  assert.equal(formatPrice(1), '$0.01');
  assert.equal(formatPrice(10), '$0.10');
  assert.equal(formatPrice(999), '$9.99');
  assert.equal(formatPrice(100), '$1.00');
});

test('separators at every thousand', () => {
  assert.equal(formatPrice(99999), '$999.99');
  assert.equal(formatPrice(100000), '$1,000.00');
  assert.equal(formatPrice(1234567), '$12,345.67');
  assert.equal(formatPrice(100000000), '$1,000,000.00');
  assert.equal(formatPrice(123456789012), '$1,234,567,890.12');
});

test('negative amounts', () => {
  assert.equal(formatPrice(-1), '-$0.01');
  assert.equal(formatPrice(-99), '-$0.99');
  assert.equal(formatPrice(-123456789), '-$1,234,567.89');
  assert.equal(formatPrice(-250000, 'EUR'), '-€2,500.00');
});

test('zero has no sign', () => {
  assert.equal(formatPrice(0, 'EUR'), '€0.00');
});

test('other currencies with separators', () => {
  assert.equal(formatPrice(1234500, 'EUR'), '€12,345.00');
  assert.equal(formatPrice(250000, 'GBP'), '£2,500.00');
});
