"""Every consumer of the Lisbon time helpers, around both clock changes, 2025-2028.

The expected values come from an independent reference: the real change dates
(checked against the tz database when this test was written) and the data
files themselves.
"""

import csv
import unittest
from collections import defaultdict
from datetime import date, datetime, time, timedelta
from pathlib import Path

from api.serializers import local_iso
from api.usage import UsageMeter, hourly_counts, usage_window
from billing.due_dates import due_at, due_day, is_overdue
from billing.dunning import reminder_times
from billing.proration import days_between, prorate
from core.models import Subscription
from core.renewals import next_renewal, renewals_between
from core.store import Store
from reports.churn import cancellations
from reports.daily import daily_totals
from reports.monthly import monthly_report
from utils.periods import day_bounds, day_length, month_bounds, week_bounds
from utils.tz import local_date, to_local, to_utc, utc_offset

# Summer time in Lisbon: (last Sunday of March, last Sunday of October), changes at 01:00 UTC.
CHANGES = {
    2024: (date(2024, 3, 31), date(2024, 10, 27)),
    2025: (date(2025, 3, 30), date(2025, 10, 26)),
    2026: (date(2026, 3, 29), date(2026, 10, 25)),
    2027: (date(2027, 3, 28), date(2027, 10, 31)),
    2028: (date(2028, 3, 26), date(2028, 10, 29)),
    2029: (date(2029, 3, 25), date(2029, 10, 28)),
}
YEARS = (2025, 2026, 2027, 2028)
H = timedelta(hours=1)


def ref_offset(utc):
    s, e = CHANGES[utc.year]
    return H if datetime.combine(s, time(1)) <= utc < datetime.combine(e, time(1)) else timedelta(0)


def ref_local(utc):
    return utc + ref_offset(utc)


def ref_utc(local):
    """For wall-clock times that exist exactly once."""
    if ref_offset(local - H) == H:
        return local - H
    return local


def ref_midnight(day):
    return ref_utc(datetime.combine(day, time(0)))


def around_changes(year, before=4, after=9):
    """Days around both changes of a year."""
    s, e = CHANGES[year]
    for change in (s, e):
        for n in range(-before, after + 1):
            yield change + timedelta(days=n)


def month_edges(year):
    """Days next to the 1st of April, October and November."""
    for m in (3, 4, 10, 11):
        first = date(year, m, 1)
        for n in (-2, -1, 0, 1):
            yield first + timedelta(days=n)


class ClockHelpersHidden(unittest.TestCase):
    def test_offsets_and_local_times(self):
        for y in YEARS:
            s, e = CHANGES[y]
            for change in (s, e):
                t = datetime.combine(change, time(0)) - timedelta(days=9)
                stop = datetime.combine(change, time(0)) + timedelta(days=10)
                while t < stop:
                    self.assertEqual(utc_offset(t), ref_offset(t), f"offset at {t} UTC")
                    self.assertEqual(to_local(t), ref_local(t), f"local time of {t} UTC")
                    self.assertEqual(local_date(t), ref_local(t).date(), f"local day of {t} UTC")
                    t += timedelta(minutes=30)

    def test_exact_change_instants(self):
        for y in YEARS:
            s, e = CHANGES[y]
            self.assertEqual(to_local(datetime.combine(s, time(0, 59))), datetime.combine(s, time(0, 59)))
            self.assertEqual(to_local(datetime.combine(s, time(1, 0))), datetime.combine(s, time(2, 0)))
            self.assertEqual(to_local(datetime.combine(e, time(0, 59))), datetime.combine(e, time(1, 59)))
            self.assertEqual(to_local(datetime.combine(e, time(1, 0))), datetime.combine(e, time(1, 0)))

    def test_local_to_utc(self):
        for y in YEARS:
            for day in list(around_changes(y)) + list(month_edges(y)):
                for hm in ((0, 0), (0, 30), (3, 0), (9, 0), (12, 45), (23, 0), (23, 59)):
                    local = datetime.combine(day, time(*hm))
                    self.assertEqual(to_utc(local), ref_utc(local), f"UTC of {local} Lisbon")

    def test_periods(self):
        for y in YEARS:
            s, e = CHANGES[y]
            for day in list(around_changes(y)) + list(month_edges(y)):
                self.assertEqual(day_bounds(day), (ref_midnight(day), ref_midnight(day + timedelta(days=1))), str(day))
            self.assertEqual(day_length(s), timedelta(hours=23))
            self.assertEqual(day_length(e), timedelta(hours=25))
            self.assertEqual(day_length(e - timedelta(days=7)), timedelta(hours=24))
            self.assertEqual(day_length(e + timedelta(days=1)), timedelta(hours=24))
            monday = e - timedelta(days=6)
            self.assertEqual(week_bounds(e), (ref_midnight(monday), ref_midnight(monday + timedelta(days=7))))
            for m in range(1, 13):
                nxt = date(y + 1, 1, 1) if m == 12 else date(y, m + 1, 1)
                self.assertEqual(month_bounds(y, m), (ref_midnight(date(y, m, 1)), ref_midnight(nxt)), f"{y}-{m:02d}")


def payments_from_data():
    path = Path(__file__).resolve().parent.parent / "data" / "payments.csv"
    with open(path, newline="", encoding="utf-8") as f:
        rows = list(csv.DictReader(f))
    return [(datetime.strptime(r["paid_at"], "%Y-%m-%dT%H:%M:%SZ"), int(r["amount_cents"])) for r in rows]


class ReportsHidden(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.store = Store.load()
        cls.by_month = defaultdict(lambda: [0, 0])
        cls.by_day = defaultdict(int)
        for at, cents in payments_from_data():
            local = ref_local(at)
            cls.by_month[(local.year, local.month)][0] += cents
            cls.by_month[(local.year, local.month)][1] += 1
            cls.by_day[local.date()] += cents

    def test_every_month(self):
        for y in YEARS:
            for m in range(1, 13):
                r = monthly_report(self.store, y, m)
                self.assertEqual([r.total_cents, r.payment_count], self.by_month[(y, m)], f"report {y}-{m:02d}")

    def test_daily_totals_in_change_months(self):
        for y in YEARS:
            for m in (3, 4, 10, 11):
                got = daily_totals(self.store, y, m)
                want = [(d, self.by_day[d]) for d, _ in got]
                self.assertEqual(got, want, f"daily totals {y}-{m:02d}")
                self.assertEqual(sum(c for _, c in got), self.by_month[(y, m)][0])

    def test_cancellations(self):
        for y in YEARS:
            for m in range(1, 13):
                got = {s.id for s in cancellations(self.store, y, m)}
                want = {
                    s.id
                    for s in self.store.subscriptions.values()
                    if s.cancelled_at is not None and (ref_local(s.cancelled_at).year, ref_local(s.cancelled_at).month) == (y, m)
                }
                self.assertEqual(got, want, f"cancellations {y}-{m:02d}")


class BillingHidden(unittest.TestCase):
    def issue_times(self):
        for y in YEARS:
            for day in list(around_changes(y, before=20, after=20)) + list(month_edges(y)):
                for hm in ((0, 30), (10, 0), (22, 30), (23, 30)):
                    yield datetime.combine(day, time(*hm))

    def test_due_dates(self):
        for issued in self.issue_times():
            want_day = ref_local(issued).date() + timedelta(days=14)
            want = ref_utc(datetime.combine(want_day, time(23, 59, 59)))
            self.assertEqual(due_day(issued), want_day, f"due day of an invoice issued {issued} UTC")
            self.assertEqual(due_at(issued), want, f"due time of an invoice issued {issued} UTC")
            self.assertFalse(is_overdue(issued, want))
            self.assertTrue(is_overdue(issued, want + timedelta(seconds=1)))

    def test_reminders(self):
        for issued in self.issue_times():
            day = ref_local(issued).date() + timedelta(days=14)
            want = [ref_utc(datetime.combine(day + timedelta(days=n), time(9))) for n in (1, 7, 14)]
            self.assertEqual(reminder_times(issued), want, f"reminders of an invoice issued {issued} UTC")

    def test_proration(self):
        for y in YEARS:
            for first in (date(y, 3, 15), date(y, 10, 15)):
                start = ref_midnight(first)
                end = ref_midnight(first + timedelta(days=30))
                for n in range(0, 30):
                    for hm in ((0, 10), (12, 0), (23, 20)):
                        changed = datetime.combine(first + timedelta(days=n), time(*hm))
                        left = (first + timedelta(days=30) - ref_local(changed).date()).days
                        if not 0 <= left <= 30:
                            continue
                        self.assertEqual(days_between(changed, end), left, f"days left after {changed} UTC")
                        got = prorate(2190, 5490, changed, start, end)
                        self.assertEqual((got["days_total"], got["days_left"]), (30, left), f"prorate at {changed} UTC")


class ApiHidden(unittest.TestCase):
    def test_usage_windows(self):
        for y in YEARS:
            for day in list(around_changes(y)) + list(month_edges(y)):
                for hm in ((0, 15), (11, 0), (22, 45), (23, 15)):
                    now = datetime.combine(day, time(*hm))
                    d = ref_local(now).date()
                    want = (ref_midnight(d), ref_midnight(d + timedelta(days=1)))
                    self.assertEqual(usage_window(now), want, f"usage window at {now} UTC")

    def test_quota_resets_at_lisbon_midnight(self):
        for y in YEARS:
            s, e = CHANGES[y]
            for day in (e + timedelta(days=2), date(y, 11, 1), date(y, 4, 1), e):
                meter = UsageMeter(quota=2)
                now = datetime.combine(day, time(12))
                self.assertTrue(meter.allow(1, now))
                self.assertTrue(meter.allow(1, now))
                self.assertFalse(meter.allow(1, now))
                reset = ref_midnight(ref_local(now).date() + timedelta(days=1))
                self.assertEqual(meter.resets_at(now), reset, f"reset after {now} UTC")
                self.assertFalse(meter.allow(1, reset - timedelta(seconds=1)))
                self.assertTrue(meter.allow(1, reset))

    def test_hourly_counts(self):
        for y in YEARS:
            for day in around_changes(y, before=1, after=8):
                start = ref_midnight(day)
                calls = [start + timedelta(minutes=20 + 60 * k) for k in range(0, 26)]
                want = defaultdict(int)
                for t in calls:
                    if ref_local(t).date() == day:
                        want[ref_local(t).hour] += 1
                self.assertEqual(hourly_counts(calls, day), dict(sorted(want.items())), f"hourly counts {day}")

    def test_local_iso(self):
        for y in YEARS:
            for day in around_changes(y):
                t = datetime.combine(day, time(13, 5))
                off = "+01:00" if ref_offset(t) == H else "+00:00"
                self.assertEqual(local_iso(t), ref_local(t).strftime("%Y-%m-%dT%H:%M:%S") + off)


class RenewalsHidden(unittest.TestCase):
    def test_next_renewal(self):
        times = ((0, 0), (0, 40), (7, 30), (12, 0), (23, 10), (23, 59))
        for day in (1, 2, 24, 25, 26, 27, 28, 29, 30, 31):
            for hm in times:
                s = Subscription(9, 9, "filter", datetime(2024, 12, 1), day, time(*hm))
                for y in YEARS:
                    for m in (3, 4, 10, 11):
                        after = ref_midnight(date(y, m, 1)) - timedelta(seconds=1)
                        last = (date(y, m + 1, 1) - timedelta(days=1)).day
                        local = datetime.combine(date(y, m, min(day, last)), time(*hm))
                        want = ref_utc(local)
                        self.assertEqual(next_renewal(s, after), want, f"renewal on day {day} at {hm} in {y}-{m:02d}")

    def test_renewals_between(self):
        s = Subscription(9, 9, "office", datetime(2024, 12, 1), 31, time(23, 30))
        for y in YEARS:
            start, end = ref_midnight(date(y, 1, 1)), ref_midnight(date(y + 1, 1, 1))
            got = renewals_between(s, start, end)
            want = []
            for m in range(1, 13):
                last = (date(y + (m == 12), m % 12 + 1, 1) - timedelta(days=1)).day
                want.append(ref_utc(datetime(y, m, last, 23, 30)))
            self.assertEqual(got, want, f"renewals in {y}")


if __name__ == "__main__":
    unittest.main()
