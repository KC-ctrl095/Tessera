# The monthly trip report

`python3 -m tripstats data/trips.csv data/stations.json "September 2026"` prints
the report. Finance signs off the report each month; `expected/report.txt` is
the signed-off September report and `run_e2e.py` compares against it.

Stages:

1. **extract** — the export is a standard CSV file (header row; fields that
   contain commas are quoted).
2. **clean** — times are ISO-8601 with `Z` or an offset such as `+01:00`; all
   times are compared in UTC. Station names are trimmed and runs of spaces
   collapsed. Trips shorter than 60 seconds or longer than 24 hours are dropped
   and counted.
3. **dedupe** — the export repeats a trip when it was corrected later; the last
   row for a trip id is the right one. Trips keep the position of their first
   appearance.
4. **enrich** — districts come from `stations.json`; station names in the
   export may differ in upper/lower case and spacing. A station that isn't in
   the list is in district "Unknown".
5. **aggregate** — per district: trips started and ended there, share of
   casual riders, kilometres, median trip time (for an even number of trips,
   the mean of the two middle values) and the busiest hour of starts (UTC;
   the earliest hour wins a tie). Per station: trips started and ended.
6. **render** — districts by trips started (most first), stations by trips
   started, then trips ended (most first); ties in alphabetical order.
