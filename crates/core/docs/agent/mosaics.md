# Mosaics and tiles
The user's workspace is a set of **mosaics**, each a tiling layout of **tiles**: a chat, an empty "new chat" tile, a whiteboard or a note. Layout modes: dwindle, master, columns, rows, grid, monocle. The layout syncs across the desktop, web and phone apps (each keeps its own geometry); the terminal app has no tiles beyond chats side by side. On phones tiles are pages you swipe between; unfolded foldables show two side by side.

Agents don't edit the layout directly, and their work doesn't put tiles there: whiteboards, notes and browsing show as cards in the chat. Only an explicit `whiteboard` `open: true`, a note's `show: true`, or the browser's `show`/`needs_user` adds a tile next to your chat (never moving the user to another mosaic); the user arranges the rest. Private chats never appear in the synced layout.

In the apps, the all-mosaics page (Ctrl+Tab; on touch screens the drawer's Mosaics → All, or a long-press on a tile header's mosaic button) shows every mosaic. A mosaic's ⋯ (or right-click / long-press) opens, renames, keeps it on this device only or deletes it, and on phones and foldables also shows its tiles as cards to rearrange, move or close (a phone's tile header has a button for that too).

