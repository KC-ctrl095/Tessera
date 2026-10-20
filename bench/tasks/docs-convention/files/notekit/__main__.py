"""The `python3 -m notekit` entry point."""

import argparse
import sys

from notekit import __version__
from notekit.commands import COMMANDS


def build_parser():
    parser = argparse.ArgumentParser(prog="notekit", description="Small tools for a folder of markdown notes.")
    parser.add_argument("--dir", default="notes", help="the notes folder (default: ./notes)")
    parser.add_argument("--version", action="version", version=f"notekit {__version__}")
    sub = parser.add_subparsers(dest="command", metavar="<command>")
    sub.required = True
    for name, module in COMMANDS.items():
        help_text = (module.__doc__ or "").strip().splitlines()[0]
        command = sub.add_parser(name, help=help_text, description=help_text)
        if hasattr(module, "add_arguments"):
            module.add_arguments(command)
        command.set_defaults(run=module.run)
    return parser


def main(argv=None):
    args = build_parser().parse_args(argv)
    return args.run(args)


if __name__ == "__main__":
    sys.exit(main())
