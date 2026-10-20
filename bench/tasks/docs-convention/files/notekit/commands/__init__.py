"""The commands notekit knows, by name (see CONTRIBUTING.md)."""

from notekit.commands import list as list_command
from notekit.commands import show

COMMANDS = {
    "list": list_command,
    "show": show,
}
