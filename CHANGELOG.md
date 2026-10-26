# Changelog

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
