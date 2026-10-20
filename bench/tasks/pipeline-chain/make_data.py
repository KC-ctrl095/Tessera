"""Trips and stations for the pipeline task: python3 make_data.py SEED OUTDIR"""

import json
import random
import sys
from datetime import datetime, timedelta, timezone

DISTRICTS = ["Alfama", "Avenidas Novas", "Baixa", "Belém", "Campo de Ourique", "Estrela", "Graça", "Lumiar",
             "Marvila", "Parque das Nações", "Santos", "Areeiro"]
STREETS = ["Rua Augusta", "Rua da Prata", "Praça do Comércio", "Cais do Sodré", "Rua do Ouro", "Largo do Carmo",
           "Avenida da Liberdade", "Praça da Figueira", "Rua das Janelas Verdes", "Largo da Graça", "Rua de Belém",
           "Avenida de Roma", "Rua Morais Soares", "Praça de Londres", "Avenida Almirante Reis", "Rua Ferreira Borges",
           "Jardim da Estrela", "Rua de São Bento", "Avenida Infante Santo", "Doca de Alcântara", "Rua da Junqueira",
           "Alameda dos Oceanos", "Rossio dos Olivais", "Rua do Açúcar", "Calçada do Combro", "Largo do Rato",
           "Campo Grande", "Alameda das Linhas de Torres", "Rua do Alecrim", "Miradouro de Santa Luzia"]
SUFFIXES = ["", ", East", ", West", " North", " (Metro)", ", Kiosk"]


def main(seed, outdir):
    rng = random.Random(seed)
    names = []
    while len(names) < 110:
        n = rng.choice(STREETS) + rng.choice(SUFFIXES)
        if n not in names:
            names.append(n)
    stations = [{"id": f"S{i + 1:03d}", "name": n, "district": rng.choice(DISTRICTS), "capacity": rng.choice([12, 15, 20, 24, 30])}
                for i, n in enumerate(names)]
    json.dump({"stations": stations}, open(f"{outdir}/stations.json", "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    weights = [rng.random() ** 2 + 0.05 for _ in stations]
    rows = []
    base = datetime(2026, 9, 1, tzinfo=timezone.utc)
    tid = 0

    def fmt(t):
        if rng.random() < 0.3:
            return (t + timedelta(hours=1)).strftime("%Y-%m-%dT%H:%M:%S+01:00")
        return t.strftime("%Y-%m-%dT%H:%M:%SZ")

    def typed(name):
        roll = rng.random()
        if roll < 0.08:
            return "  " + name.lower() + " "
        if roll < 0.14:
            return name.upper()
        if roll < 0.18:
            return name.replace(" ", "  ", 1)
        return name

    for _ in range(3200):
        tid += 1
        start = base + timedelta(seconds=rng.randrange(0, 30 * 86400))
        roll = rng.random()
        if roll < 0.03:
            secs = rng.randint(5, 59)
        elif roll < 0.045:
            secs = rng.randint(24 * 3600 + 1, 40 * 3600)
        elif roll < 0.09:
            secs = rng.choice([rng.randint(60, 3540), 3600 + rng.randint(-50, 50)])
        else:
            secs = int(rng.lognormvariate(6.6, 0.6)) + 60
        a, b = rng.choices(stations, weights)[0], rng.choices(stations, weights)[0]
        sa = "Depot" if rng.random() < 0.01 else typed(a["name"])
        row = [f"T{tid:05d}", fmt(start), fmt(start + timedelta(seconds=secs)), sa, typed(b["name"]),
               f"B{rng.randint(100, 999)}", "casual" if rng.random() < 0.3 else "member", str(rng.randint(300, 9000))]
        rows.append(row)
        if rng.random() < 0.04:
            fixed = list(row)
            fixed[2] = fmt(start + timedelta(seconds=secs + rng.randint(30, 900)))
            fixed[7] = str(int(row[7]) + rng.randint(100, 2000))
            rows.append(("later", fixed))
    # Corrected rows come later in the export.
    final, later = [], []
    for r in rows:
        if isinstance(r, tuple):
            later.append(r[1])
        else:
            final.append(r)
            if later and rng.random() < 0.2:
                final.append(later.pop(0))
    final += later
    import csv

    with open(f"{outdir}/trips.csv", "w", newline="", encoding="utf-8") as f:
        w = csv.writer(f)
        w.writerow(["trip_id", "started_at", "ended_at", "start_station", "end_station", "bike_id", "member_type", "distance_m"])
        w.writerows(final)


if __name__ == "__main__":
    main(int(sys.argv[1]), sys.argv[2])
