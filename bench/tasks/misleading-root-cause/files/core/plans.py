"""Plan rules: which changes are upgrades, and what a change costs."""

ORDER = ["espresso", "filter", "sampler", "office"]


def is_upgrade(old_code: str, new_code: str) -> bool:
    """Moving to a plan further down ORDER is an upgrade."""
    return ORDER.index(new_code) > ORDER.index(old_code)


def bags_delta(store, old_code: str, new_code: str) -> int:
    return store.plan(new_code).bags_per_month - store.plan(old_code).bags_per_month
