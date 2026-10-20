"""Copies tickets from the help desk API, page by page."""

import json
import time
import urllib.request

import config


def get_page(page, opener=urllib.request.urlopen, sleep=time.sleep):
    """One page of tickets as a list of dicts.

    A request that fails is tried again up to config.RETRIES times, waiting
    config.BACKOFF seconds before the first retry and twice as long each time.
    """
    url = f"{config.API_URL}/tickets?page={page}&per_page={config.PAGE_SIZE}"
    req = urllib.request.Request(url, headers={"User-Agent": config.USER_AGENT})
    delay = config.BACKOFF
    for attempt in range(config.RETRIES + 1):
        try:
            with opener(req, timeout=config.TIMEOUT) as resp:
                return json.load(resp)["tickets"]
        except OSError:
            if attempt == config.RETRIES:
                raise
            sleep(delay)
            delay *= 2


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
