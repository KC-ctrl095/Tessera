# Changelog

All notable changes to ticket-sync, newest first.

## 2026-09-04

- Failed requests are retried up to 5 times, waiting 1.5 seconds before the first retry and twice as long after each one.

## 2026-08-20

- First release: copies new and updated tickets from the help desk API every five minutes.
