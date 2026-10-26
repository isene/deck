# Changelog

## 0.1.5 (2026-10-26)

- C-Y copies the last answer as drawn, HyperList and all. C-S-Y copies
  what the answer pane shows, as C-Y did before.
- C-B picks the answer pane's colour in prism and keeps it in
  ~/.fleetrc, where fleet's b keeps it. A one-column pane between the
  answer and the log takes the same colour.
- The prompt grows to as many rows as it needs, up to a third of the
  screen, and then scrolls. Up and Down move between its rows. `\` then
  Enter, or C-J, starts a new line, as in Claude Code.
- C-? shows the keys in a popup. The bottom row shows deck's version at
  the right, and says so when the binary was rebuilt.
- The window is titled "Deck #tag".
- A resize no longer draws every answer again; C-L still does.

## 0.1.4 (2026-10-26)

- deck draws the HyperList itself, at the answer pane's width, so a long
  Item wraps with its "+" mark. Claude Code's style hook leaves the text
  plain when deck sets HL_RAW.
- C-Y copies what the answer pane shows (OSC 52).
- C-W and C-S-W make the answer pane a tenth wider or narrower, from half
  to nine tenths; the width is kept in ~/.cache/deck.json. Word delete
  stays on C-Backspace.

## 0.1.3 (2026-10-26)

- C-G opens the prompt in `$EDITOR` (scribe if unset), as in Claude Code.

## 0.1.2 (2026-10-26)

- Answers are drawn once: Claude Code has run its display hook on them
  already, and a second HyperList pass garbled the colour codes.
- The answer pane takes the session's background from fleet's ~/.fleetrc.
- The context window of each model is remembered in ~/.cache/deck.json, so
  the status line shows the context share before the first turn.
- A status line ready before the key reader started no longer waits for a
  key.

## 0.1.1 (2026-10-26)

- The status line has a dark grey background, apart from the answer pane.

## 0.1.0 (2026-10-26)

- First version: the answer pane, the tool log, the info bar and the status
  line, over `claude -p` in stream-json.
- A stop hook's rewrite shows as the answer, with one log line saying so.
- Permission questions are answered with y or n on the input line.
- `deck <tag>` and `deck <id>` resume a session and show its last answer.
