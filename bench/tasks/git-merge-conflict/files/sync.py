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
    """Every ticket, page by page, until a page comes back short."""
    tickets = []
    page = 1
    while True:
        batch = get_page(page, opener)
        tickets.extend(batch)
        if len(batch) < config.PAGE_SIZE:
            return tickets
        page += 1
