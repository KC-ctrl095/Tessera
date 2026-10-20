"""python3 -m tripstats TRIPS.csv STATIONS.json "September 2026" > report.txt"""

import sys

from .aggregate import aggregate
from .clean import clean
from .dedupe import dedupe
from .enrich import enrich, load_stations
from .extract import read_trips
from .render import render


def main(argv):
    trips_path, stations_path, month = argv
    rows = read_trips(trips_path)
    trips, dropped = clean(rows)
    trips, replaced = dedupe(trips)
    trips = enrich(trips, load_stations(stations_path))
    districts, stations = aggregate(trips)
    sys.stdout.write(render(month, len(trips), dropped, replaced, districts, stations))


if __name__ == "__main__":
    main(sys.argv[1:])
