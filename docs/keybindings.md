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
| `^R` | Save the current search and open fuzzy-filtered search history |
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

In `^F`, **Search content** (`v`) cycles Visible / All / Hidden with Space or a
repeated mnemonic. Visible is the interactive default and follows the Visibility
toggles. `all:`, `visible:`, and `hidden:` in the query temporarily override this
selection. `^R` resets it to Visible; Escape cancels modal edits.

## Search history

`^R` immediately saves the current nonblank query, bypassing the automatic-save
dwell, and opens history with that query as its filter. Type to filter using
nucleo's fzf-style matching: spaces separate terms, with smart case and support
for prefix (`^`), suffix (`$`), exact (`'`), and exclusion (`!`) patterns.
Matching searches are ranked by score, then newest first. Clear the filter to
list all searches newest first.

Use `↑` / `↓` or `^P` / `^N` to select and `PgUp` / `PgDn` to page. `Enter`
recalls the selected query and runs it immediately. Mouse scrolling and clicking
select entries; double-click recalls. `Esc` cancels without changing the main
query or its cursor. Editing the history filter does not record new searches.

History covers the main search, including rules preview, but not the viewer's
find text or noninteractive commands. `^R` does nothing on the main screen when
`history_save_count` is zero. Its reset action inside Filters is unchanged.

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
| `Ctrl+left-click` or `Alt+Ctrl+left-click` | Toggle a block and set the range anchor |
| `Shift+left-click` or `Alt+Shift+left-click` | Select the inclusive range from the anchor, replacing the selection |
| `Ctrl+Shift+left-click` or `Alt+Ctrl+Shift+left-click` | Add the inclusive anchored range to the selection |
| `Alt+C` | Copy selected blocks as source Markdown, in conversation order |

Blocks include messages, tool events, reasoning, plans, summaries, session context,
and metrics. Markdown headings within a message remain part of that message.
Selection survives scrolling, searching, resizing, and temporary overlays. Range
selection includes offscreen blocks and skips hidden blocks. Without an anchor,
Shift-click or Alt+Shift-click selects just the clicked block. Alt-click alone is
ignored. Alt is optional for toggle/range gestures, and both forms share the same
selection and anchor.

The terminal must forward modified mouse events to AICS. Terminal and OS shortcuts
can intercept these gestures: some terminals reserve Shift-click for local text
selection, [iTerm2 uses Option/Alt to disable mouse reporting](https://iterm2.com/documentation-preferences-profiles-terminal.html#enable-mouse-reporting),
and [Kitty reserves Alt+Ctrl+Shift-click for rectangular selection](https://sw.kovidgoyal.net/kitty/conf/#mouse-actions).
The Alt alternatives work where those combinations are forwarded; they do not
override terminal or OS bindings.

Copying includes role/tool headings and timestamps, preserves source Markdown and
code indentation, and respects display filters (including hidden command output).
Copying keeps the selection; an empty selection leaves the clipboard unchanged.

Applying or saving filters from the viewer returns to the same session. When
display options change, the viewer returns to the beginning of the block at the
old top of the conversation area. If that block is hidden, it chooses the surviving
block nearest that position before filtering, preferring the following block on
a tie. The normal bottom scroll limit still applies. If no formerly visible blocks
remain, the viewer starts at the beginning of the new content. Changes affecting
only session search results preserve the existing scroll position.

Blocks that become hidden are deselected. If the updated search filters exclude
the open session, a dialog offers **Close session** and **Keep session open**,
with Keep focused by default:

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
