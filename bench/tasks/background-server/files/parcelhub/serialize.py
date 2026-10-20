"""Parcels as JSON."""


def summary(p):
    return {
        "id": p.id,
        "status": p.status,
        "recipient": p.recipient,
        "city": p.city,
        "country": p.country,
        "updated": p.events[-1]["at"] if p.events else None,
    }


def detail(p):
    d = summary(p)
    d.update(postcode=p.postcode, weight_g=p.weight_g, events=list(p.events))
    return d
