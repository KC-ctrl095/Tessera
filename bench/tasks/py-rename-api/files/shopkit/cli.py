"""python3 -m shopkit.cli <user id>: print a user and their orders."""

import sys

from shopkit import get_usr, orders_for


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if len(argv) != 1 or not argv[0].isdigit():
        print("usage: python3 -m shopkit.cli USER_ID", file=sys.stderr)
        return 2
    user = get_usr(int(argv[0]))
    print(user.display())
    for order in orders_for(user.id):
        print(f"  order {order.id}: {order.total_cents / 100:.2f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
