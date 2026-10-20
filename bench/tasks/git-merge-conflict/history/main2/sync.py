"""Copies tickets from the help desk API, page by page."""

import json
import urllib.request

import config


def get_page(page, opener=urllib.request.urlopen):
    """One page of tickets as a list of dicts."""
    url = f"{config.API_URL}/tickets?page={page}&per_page={config.PAGE_SIZE}"
    req = urllib.request.Request(url, headers={"User-Agent": config.USER_AGENT})
    with opener(req, timeout=config.TIMEOUT) as resp:
        return json.load(resp)["tickets"]


def fetch_all(opener=urllib.request.urlopen):
    """Every ticket, page by page, until a page comes back short.

    A ticket that is updated while we page through can move to a later page
    and come back twice; the later copy wins.
    """
    tickets = {}
    page = 1
    while True:
        batch = get_page(page, opener)
        for ticket in batch:
            tickets.pop(ticket["id"], None)
            tickets[ticket["id"]] = ticket
        if len(batch) < config.PAGE_SIZE:
            return list(tickets.values())
        page += 1
