# TUI keybindings

Press `?` or `Ctrl+L` from the session list to open the contextual help. The
help modal is searchable and includes separate Session List, Viewer, and Search
Query tabs.

In the tables below, `^` means Ctrl.

## Session list

| Key | Action |
| --- | --- |
| Type | Edit the search query |
| `↑` / `↓` or `^J` / `^K` | Move the selected session |
| `PgUp` / `PgDn` | Scroll the preview, or page through the list when preview is hidden |
| `Home` / `End` | Jump within the preview, or to the first/last result when preview is hidden |
| `⏎` | Open the selected session's actions menu |
| `^F` | Open filters and display options; `^S` in the modal applies them and saves them as startup defaults |
| `^G` | Toggle between global and current-directory scope |
| `^S` | Open settings |
| `^T` | Show or hide the preview panel |
| `^N` / `^P` | Jump to the next or previous highlighted preview match |
| `Shift+↑` / `Shift+↓` | Jump to the previous or next message/event in the preview |
| `^Shift+↑` / `^Shift+↓` | Jump to the previous or next user message in the preview |
| `^Y` | Cycle the session-card snippet between session text and available summaries |
| `^D` | Move the selected session to AICS trash; in rules preview, process marked proposals |
| `Shift+←` / `Shift+→` | Resize the list/preview split |
| `?` / `^L` | Open contextual help |
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
| `^D` | Move the current session to AICS trash |
| `⏎` | Open the current session's actions menu |
| `?` | Open contextual help on the Viewer tab |
| `Esc` | Close the viewer |
| Mouse wheel | Scroll the conversation |

Modal-specific key hints appear at the bottom of each modal. `Esc` generally
cancels or closes a modal, while `Ctrl+C` exits the TUI from any screen.

[Back to the README.](../README.md#keybindings-tui)
