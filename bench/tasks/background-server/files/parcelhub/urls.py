"""URLs the API hands out (Link headers)."""


def build_url(path, params):
    """path?name=value&… with the parameters in the given order."""
    if not params:
        return path
    return path + "?" + "&".join(f"{name}={value}" for name, value in params.items())


def link_header(base, links):
    """Link header value for [(url path, rel), …]."""
    return ", ".join(f'<{base}{url}>; rel="{rel}"' for url, rel in links)
