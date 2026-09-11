# TUI keybindings

Press `Ctrl+L` from the session list to open the contextual help. The help modal
is searchable and includes separate Session List, Viewer, and Search Query tabs.

In the tables below, `^` means Ctrl.

## Session list

| Key | Action |
| --- | --- |
| Type | Edit the search query |
| `↑` / `↓` or `^J` / `^K` | Move the selected session |
| `PgUp` / `PgDn` | Scroll the preview, or page through the list when preview is hidden |
| `Home` / `End` | Jump within the preview, or to the first/last result when preview is hidden |
| `⏎` | Open the selected session's actions menu, including complete and active-filtered `.txt` exports and a `rules.js`-shaped JSON export |
| `^F` | Open filters and display options; `^S` in the modal applies them and saves them as startup defaults |
| `^G` | Toggle between global and current-directory scope |
| `^S` | Open settings |
| `^T` | Show or hide the preview panel |
| `^N` / `^P` | Jump to the next or previous highlighted preview match |
| `Shift+↑` / `Shift+↓` | Jump to the previous or next message/event in the preview |
| `^Shift+↑` / `^Shift+↓` | Jump to the previous or next user message in the preview |
| `^Y` | Cycle the session-card snippet between session text and available summaries |
| `^D` | Move the selected session to AICS trash, including the complete local Antigravity bundle; in rules preview, process marked proposals |
| `Shift+←` / `Shift+→` | Resize the list/preview split |
| `^L` | Open contextual help |
| `Esc` | Clear a non-empty query; quit when the query is empty |
| `^C` | Quit immediately |
| Double click | Open a session directly in the full viewer |
| Mouse wheel | Scroll the session list or preview under the pointer |

## Session viewer

| Key | Action |
| --- | --- |
| Type | Edit the viewer's inline search query |
| `↑` / `↓` | Scroll one line |
| `PgUp` / `PgDn` | Scroll one page |
| `Home` / `End` | Jump to the top or bottom |
| `Shift+↑` / `Shift+↓` | Jump to the previous or next message/event |
| `^Shift+↑` / `^Shift+↓` | Jump to the previous or next user message |
| `^N` / `^P` | Jump to the next or previous highlighted match |
| `^U` / `^E` | Use readline-style editing in the search box |
| `^F` | Open filters and display options |
| `^D` | Move the current session to AICS trash, including the complete local Antigravity bundle |
| `⏎` | Open the current session's actions menu |
| `^L` | Open contextual help on the Viewer tab |
| `Esc` | Close the viewer |
| Mouse wheel | Scroll the conversation |
| Left-click | Select one whole block; click empty conversation space to clear selection |
| `Ctrl+left-click` | Toggle a block and set the range anchor |
| `Shift+left-click` | Select the inclusive range from the anchor, replacing the selection |
| `Ctrl+Shift+left-click` | Add the inclusive anchored range to the selection |
| `Alt+C` | Copy selected blocks as source Markdown, in conversation order |

Blocks include messages, tool events, reasoning, plans, summaries, session context,
and metrics. Markdown headings within a message remain part of that message.
Selection survives scrolling, searching, resizing, and temporary overlays. Range
selection includes offscreen blocks and skips hidden blocks. Without an anchor,
Shift-click selects just the clicked block. Some terminals reserve Shift-click for
native text selection; these terminals must forward modified mouse events to AICS
for Shift-click block selection to work.

Copying includes role/tool headings and timestamps, preserves source Markdown and
code indentation, and respects display filters (including hidden command output).
Copying keeps the selection; an empty selection leaves the clipboard unchanged.

Applying or saving filters from the viewer returns to the same session. Blocks
that become hidden are deselected. If the updated search filters exclude the open
session, a dialog offers **Close session** and **Keep session open**, with Keep
focused by default:

- Tab/Shift+Tab switches between the buttons; Enter executes the focused button.
- `k`/`K` keeps the session open; `c`/`C` closes it and returns to filtered results.
- Space or `r`/`R` toggles **Remember my choice**. Clicking its checkbox or label also toggles it.
- Clicking a button executes it. Escape keeps the session open without remembering,
  regardless of checkbox state. Applied filters remain in effect.

Remembered choices persist across restarts. See
[the settings-file preference](config-settings.md#viewer-filter-exclusion) to reset it.

Modal-specific key hints appear at the bottom of each modal. `Esc` generally
cancels or closes a modal, while `Ctrl+C` exits the TUI from any screen.

[Back to the README.](../README.md#keybindings-tui)
