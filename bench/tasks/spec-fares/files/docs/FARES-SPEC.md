# Fares specification, 2026 (river and coastal ferries)

Status: approved by the commercial team, 2026-02-20. Owner: pricing@transtejo.example.
This document is the reference for `fares.quote()`. When the code and this
document disagree, this document wins. Section numbers are referred to in
support tickets, so keep them stable.

## 0. Vocabulary

- **Booking**: what one customer buys in one go: one route, one class, one
  outbound trip and optionally one return trip, for a list of passengers and
  vehicles. All passengers and vehicles travel on both legs.
- **Leg**: one crossing (the outbound trip, or the return trip).
- **Adult fare**: the price of one adult on a leg, from the fare table (§2),
  for the route, class and season of that leg.
- Amounts are in euros. Work with exact decimal arithmetic (no binary floats);
  round only where this document says so. "Round to cents" always means half
  up (0.005 becomes 0.01).

## 1. Seasons

Every leg has a season, from the date of that leg (the outbound and the return
leg can be in different seasons):

1. **peak**: the public holidays of 2026 (2026-01-01, 2026-02-17, 2026-04-03, 2026-04-05, 2026-04-25, 2026-05-01, 2026-06-04, 2026-06-10, 2026-06-13, 2026-08-15, 2026-10-05, 2026-11-01, 2026-12-01, 2026-12-08), and from 24 December to
   1 January inclusive;
2. **high**: from 15 June to 15 September inclusive (unless the day is peak);
3. **low**: every other day.

## 2. The fare table

`docs/fares-2026.csv` is the fare table exported by the pricing team: one row
per route and class, with the adult fare for each season. A route without a
`comfort` row doesn't offer comfort class; booking comfort class on it is an
error. The `vehicles` column says whether the route carries vehicles; a
booking with vehicles on a route that doesn't is an error. An unknown route
code is an error. Errors are `ValueError`.

## 3. Passengers

Each passenger has a `type` and, for some types, an `age` or a `card`. The
price of a passenger on a leg starts from the adult fare of that leg:

| Type | Who | Price |
|---|---|---|
| `adult` | 12 to 64 years | the adult fare |
| `child` | 4 to 11 years | 50% of the adult fare |
| `infant` | 0 to 3 years | free, see §3.2 |
| `senior` | 65 or older | standard class: 70% of the adult fare; comfort class: the adult fare |
| `student` | holders of a student card (`"card": true`) | on weekdays (Monday to Friday): 75% of the adult fare; on weekends: the adult fare |
| `disabled` | holders of a disability card | 50% of the adult fare, in both classes |
| `companion` | travels with a disabled passenger | free, see §3.3 |

### 3.1 Ages decide

When an age is given, it decides the category, whatever the type says, for the
types `adult`, `child` and `infant`: 0 to 3 is an infant, 4 to 11 a child, 12
or more an adult. A `senior` younger than 65 pays as an adult. A `student`
without `"card": true` pays as an adult. Without an age, the type is taken as
given.

### 3.2 Infants

Each adult, senior, student or disabled passenger (after §3.1) may take one
infant along for free. Further infants pay the child price (but they don't
count as children for §5).

### 3.3 Companions

Each disabled passenger may bring one companion for free. Further companions
pay the adult fare (and count as adults for the peak-hour rule of §4, but not
for §3.2 or §5).

## 4. Time of departure

Each leg has a departure time (`HH:MM`). Applied to the passenger prices from
§3, after them, per leg. Vehicles are not affected.

- **Peak hours**: on weekdays, departures from 07:00 up to but not including
  09:30, and from 17:00 up to but not including 19:30. In **standard class**
  only, adults, students and seniors pay 15% more (on top of their price from
  §3: a student on a weekday at 08:00 pays 75% × 115% of the adult fare).
  Children, disabled passengers and free travellers are not affected.
- **Night**: departures from 00:00 up to but not including 05:00 are 20% cheaper
  for every passenger who pays, in both classes. (Night and peak hours never
  overlap.)

## 5. Families

On a leg in **standard class**, when the booking has at least two adults and
at least two children, up to three of the children travel free on that leg;
any further children pay the child price. Adults and children here are the
passengers in the adult and child categories after §3.1 (so also seniors
younger than 65 and students without a card, who pay as adults). Seniors,
students with a card, disabled passengers, companions and infants (also those
paying the child price under §3.2) don't count and never travel free under
this rule. There is no family rule in comfort class.

## 6. Vehicles

Vehicle fares per leg, the same on every route that carries vehicles:

| Vehicle | low | high | peak |
|---|---|---|---|
| `car` | 18.00 | 22.00 | 26.00 |
| `motorcycle` | 7.50 | 9.00 | 10.50 |
| `van` | the car fare, plus 3.00 for every started metre of length (`length_m`) beyond 5.0 m | | |
| `bicycle` | free | 2.00 | 2.00 |

A van of 6.4 m in high season costs 22.00 + 2 × 3.00 = 28.00 (two started
metres beyond 5.0 m). A van of 5.0 m or less costs the car fare. Any other
vehicle type is an error.

## 7. Leg subtotal

The **subtotal** of a leg is the sum of the prices of all passengers (§3 to §5)
and all vehicles (§6) on that leg, rounded to cents once, at the end (not per
passenger).

## 8. Booking discounts

Applied in this order, to the leg subtotals:

### 8.1 Groups

A booking with **10 or more paying passengers** gets 12% off every leg
subtotal (passengers and vehicles alike). Paying passengers are those whose
price on the *outbound* leg is above zero (free infants, free companions and
children who travel free under §5 don't count). The discount is computed per
leg and rounded to cents per leg.

### 8.2 Return trips

When the booking has a return leg that is at most 30 days after the outbound
leg (the same day counts as 0 days), the return leg gets 10% off its subtotal
after §8.1, rounded to cents. A return more than 30 days later gets no return
discount. A return before the outbound date is an error.

### 8.3 Promotion codes

A booking may carry one code (`promo`). Codes are case-sensitive.

| Code | Kind | Value | Minimum | Valid (outbound date) | Routes | Combines with the return discount |
|---|---|---|---|---|---|---|
| `SUMMER10` | percent | 10% | 0 EUR | 2026-06-15 to 2026-09-15 | all routes | yes |
| `TROIA5` | fixed amount | 5.00 EUR | 20.00 EUR | 2026-01-01 to 2026-12-31 | SE-TR, SE-TR-X | yes |
| `WELCOME` | percent | 15% | 10.00 EUR | 2026-01-01 to 2026-12-31 | all routes | no |
| `FAMILY` | fixed amount | 3.00 EUR | 0 EUR | 2026-01-01 to 2026-12-31 | all routes | yes |
| `ALGARVE20` | percent | 20% | 15.00 EUR | 2026-05-01 to 2026-10-31 | FA-CU, FA-FA, OL-AR, VR-AY | no |
| `NIGHTOWL` | percent | 25% | 50.00 EUR | 2026-01-01 to 2026-12-31 | LI-PO | no |
| `TEJO2` | fixed amount | 2.00 EUR | 8.00 EUR | 2026-03-01 to 2026-11-30 | CS-BA, CS-CA, CS-SE, CS-MO, TP-SE, TP-BA, BE-PO, BE-TR | yes |
| `WINTER30` | percent | 30% | 30.00 EUR | 2026-11-01 to 2027-02-28 | all routes | no |
| `STUDENT5` | percent | 5% | 0 EUR | 2026-09-01 to 2027-06-30 | all routes | yes |
| `BIKE` | fixed amount | 1.00 EUR | 0 EUR | 2026-04-01 to 2026-10-31 | all routes | yes |
| `MINHO` | percent | 12% | 5.00 EUR | 2026-01-01 to 2026-12-31 | CA-VI | yes |
| `PARTNER45` | percent | 45% | 0 EUR | 2026-01-01 to 2026-12-31 | all routes | yes |
| `EXPIRED` | percent | 50% | 0 EUR | 2025-01-01 to 2025-12-31 | all routes | yes |

- A code applies only when the outbound date is within its validity (both
  dates inclusive), the route is one of its routes, and the amount it applies
  to (see below) is at least its minimum. An unknown, expired or not
  applicable code is ignored: the quote is computed without it, and
  `promo_applied` is false. It is not an error.
- A **code that combines** with the return discount applies to the sum of the
  leg subtotals after §8.1 and §8.2.
- A **code that doesn't combine** applies to the sum of the leg subtotals after
  §8.1 only. When the booking also qualifies for a return discount (§8.2), the
  customer gets only the larger of the two: the code (then there is no return
  discount) or the return discount (then the code is not applied). When they
  are equal, the return discount is kept.
- A percentage code takes that percentage of the amount it applies to, rounded
  to cents. A fixed-amount code takes its value, but never more than the
  amount it applies to.

### 8.4 The discount cap

The discounts of §8.1 to §8.3 together never exceed 40% of the sum of the leg
subtotals (before §8.1). When they would, they are reduced to exactly that cap
(rounded to cents). The prices of §3 to §5 (child prices, free travel and so
on) are not discounts in this sense and don't count towards the cap.

## 9. Fees and payment

- **Booking fee**: 0.50 per booking sold online (`"channel": "online"`, the
  default), nothing at the counter (`"channel": "counter"`). It is added after
  all discounts and is never discounted.
- **Cash**: when `"payment": "cash"`, the final total is rounded to the nearest
  0.05 (half up: 12.325 would become 12.35, 12.32 becomes 12.30). Card payments
  (`"payment": "card"`, the default) are not rounded further.

## 10. The quote

`quote(booking)` takes a dict:

```python
{
    "route": "CS-SE",                 # required
    "class": "standard",              # or "comfort"; default "standard"
    "date": "2026-07-14",             # outbound leg, required
    "time": "08:15",                  # outbound departure, required
    "return": {"date": "2026-07-20", "time": "18:30"},   # optional
    "passengers": [{"type": "adult"}, {"type": "child", "age": 7}],   # at least one
    "vehicles": [{"type": "van", "length_m": 6.4}],      # optional
    "promo": "SUMMER10",              # optional
    "channel": "online",              # or "counter"
    "payment": "card",                # or "cash"
}
```

and returns:

```python
{
    "total": "123.45",        # what the customer pays (§9), two decimals, as a string
    "legs": [{"date": "2026-07-14", "season": "high", "subtotal": "6.79"}],   # §1, §7; outbound first
    "promo_applied": True,    # §8.3
    "booking_fee": "0.50",    # §9
}
```

A booking with no passengers is an error.

## 11. Worked examples

### A family in summer, at peak hour, with a code

```python
quote({"route": "CS-SE", "date": "2026-07-14", "time": "08:15", "passengers": [{"type": "adult"}, {"type": "adult"}, {"type": "child", "age": 7}, {"type": "child", "age": 9}, {"type": "infant", "age": 1}], "promo": "SUMMER10"})
# -> {"total": "6.61", "legs": [{"date": "2026-07-14", "season": "high", "subtotal": "6.79"}], "promo_applied": true, "booking_fee": "0.50"}
```

Tuesday 08:15 is peak hour: each adult pays 2.95 × 115%. Both children travel free (§5), the infant too (§3.2). Subtotal 6.785 → 6.79; SUMMER10 takes 0.68; plus the 0.50 booking fee.

### A night crossing by car, return in ten days, with a code that doesn't combine

```python
quote({"route": "LI-PO", "class": "comfort", "date": "2026-05-12", "time": "23:30", "return": {"date": "2026-05-22", "time": "01:15"}, "passengers": [{"type": "adult"}, {"type": "adult"}], "vehicles": [{"type": "car"}], "promo": "NIGHTOWL"})
# -> {"total": "200.30", "legs": [{"date": "2026-05-12", "season": "low", "subtotal": "146.00"}, {"date": "2026-05-22", "season": "low", "subtotal": "120.40"}], "promo_applied": true, "booking_fee": "0.50"}
```

Outbound 2 × 64.00 + 18.00 = 146.00. The return departs at 01:15 (night): 2 × 64.00 × 80% + 18.00 = 120.40, return discount 12.04. NIGHTOWL (25% of 266.40 = 66.60) doesn't combine and is larger, so it replaces the return discount.

### A group, paid in cash at the counter

```python
quote({"route": "SE-TR", "date": "2026-03-07", "time": "10:00", "passengers": [{"type": "adult"}, {"type": "adult"}, {"type": "adult"}, {"type": "adult"}, {"type": "adult"}, {"type": "adult"}, {"type": "adult"}, {"type": "adult"}, {"type": "adult"}, {"type": "senior", "age": 71}, {"type": "student", "card": true}, {"type": "infant", "age": 2}], "channel": "counter", "payment": "cash"})
# -> {"total": "40.00", "legs": [{"date": "2026-03-07", "season": "low", "subtotal": "45.48"}], "promo_applied": false, "booking_fee": "0.00"}
```

A Saturday: the student pays the adult fare. Eleven passengers pay (the infant is free), so the group gets 12% off; no booking fee at the counter; cash rounds to 0.05.

### Weekday students and seniors at peak hour

```python
quote({"route": "TP-BA", "date": "2026-10-06", "time": "18:05", "passengers": [{"type": "student", "card": true}, {"type": "senior", "age": 66}, {"type": "senior", "age": 60}]})
# -> {"total": "8.81", "legs": [{"date": "2026-10-06", "season": "low", "subtotal": "8.31"}], "promo_applied": false, "booking_fee": "0.50"}
```

Student 3.40 × 75% × 115%, senior 3.40 × 70% × 115%, and the 60-year-old 'senior' pays as an adult: 3.40 × 115%.

### A van and a bicycle

```python
quote({"route": "BE-TR", "date": "2026-08-20", "time": "12:00", "passengers": [{"type": "adult"}], "vehicles": [{"type": "van", "length_m": 6.4}, {"type": "bicycle"}]})
# -> {"total": "32.20", "legs": [{"date": "2026-08-20", "season": "high", "subtotal": "31.70"}], "promo_applied": false, "booking_fee": "0.50"}
```

High season: 1.70 + (22.00 + 2 × 3.00) + 2.00.

### The discount cap

```python
quote({"route": "CS-MO", "date": "2026-11-03", "time": "11:00", "return": {"date": "2026-11-04", "time": "11:00"}, "passengers": [{"type": "adult"}, {"type": "adult"}, {"type": "adult"}, {"type": "adult"}, {"type": "adult"}, {"type": "adult"}, {"type": "adult"}, {"type": "adult"}, {"type": "adult"}, {"type": "adult"}], "promo": "PARTNER45"})
# -> {"total": "38.90", "legs": [{"date": "2026-11-03", "season": "low", "subtotal": "32.00"}, {"date": "2026-11-04", "season": "low", "subtotal": "32.00"}], "promo_applied": true, "booking_fee": "0.50"}
```

Each leg 10 × 3.20 = 32.00. Group: 3.84 off each leg (7.68); return: 10% of 28.16 = 2.82; PARTNER45 combines: 45% of 53.50 = 24.08. Together 34.58, more than 40% of 64.00, so the discounts are capped at 25.60: 64.00 − 25.60 + 0.50.

## 12. History

- 2026-02: student discount now weekdays only (was every day); the group
  threshold went from 12 to 10 paying passengers.
- 2025-10: the discount cap (§8.4) introduced after a promotion stacked with
  group and return discounts to 70% off.
- 2025-06: bicycles are free in low season.
