# Changelog

## 0.1.37 (2026-10-31)

- Under gluon, the top bar leaves out the session's name, which the
  status line shows already.
## 0.1.36 (2026-10-31)

- The bridge look writes the top bar's text in black on the bar's
  orange, so the bar stays whole under it (red when deck is old).
  `bridge::on_bar` does this for gluon too.
## 0.1.35 (2026-10-31)

- The bridge look moves the top bar's state and the status line into
  the sidebar: the session, the model, the context, the week's limit,
  and below them what Claude is doing (working, the time, the tools,
  the step, the ETA). The top and bottom bars are whole again, with
  deck's version at the right of the top one. The status line command
  does not run in this look, which saves its forks after each turn.
## 0.1.34 (2026-10-31)

- The bridge look on deck's own screen: C-S-B draws an orange frame
  around the panes, in the style of a starship's screens, with the
  session, the model and the context on buttons in a sidebar. It needs
  glass or kitty, and it stays on for the next start while
  `~/.deck/bridge` exists. Under gluon, gluon draws its own.
- The drawing code for the look moved here from gluon, which now uses
  deck's. Off, it costs one stat() at start.
## 0.1.33 (2026-10-30)

- Under gluon, deck can start some columns in from the left, so gluon's
  Star Trek look keeps its sidebar beside the Claude pane.

## 0.1.32 (2026-10-30)

- `/model` alone opens a popup of models (opus, sonnet, haiku, fable,
  default), with the bar on the one in use. The pick goes to Claude Code
  as `/model <name>`, the same as typing it. No cost until it is used.

## 0.1.31 (2026-10-30)

- The quit popup and the key popup hide the cursor. Before, it sat as
  a block beside the popup, where the last character was drawn.

## 0.1.27 (2026-10-29)

- Under gluon, a fleet message for the session goes to Claude as it
  lands, whichever pane has the keys: deck sends "Check fleet messages"
  itself. Before, fleet typed that line into the window, and in gluon
  the keys went to the Mail tab or a box at Home. deck starts Claude
  with FLEET_SELF_WAKE=1, so fleet 0.3.74 types nothing there. While
  Claude works, the stop hook hands the message over at the end of the
  turn. Cost: one thread asleep in read() on two mailboxes; no wakeups
  while idle.

## 0.1.24 (2026-10-28)

- The top bar shows the session's open ETA, as fleet's ETA column does:
  "○ ready · ETA 16:30", and red once that time has passed. deck reads
  fleet's mark in ~/.fleet/eta/<session id> at the start and at the end
  of each turn: one small file read per turn, nothing while idle.

## 0.1.23 (2026-10-28)

- A message sent with C-Enter during a turn turns blue at once, since
  Claude has it; before, it stayed purple until Claude read it. Claude
  reads it only when its current step ends, and the bar says so: "1
  sent, read when Bash ends" in place of "1 waiting". Purple, and "N
  waiting", are for messages deck still holds.

## 0.1.22 (2026-10-28)

- Closing deck's window stops Claude at once, with its commands, a
  background job too, as y after C-D does. Before, Claude went on alone
  with no window until its turn and its jobs were done. On Linux only.

## 0.1.21 (2026-10-28)

- Under gluon, deck no longer writes "<app> screen attached" in the
  tool log at each tab switch; five or six piled up. The screen still
  goes to Claude with the next prompt.

## 0.1.20 (2026-10-28)

- C-D asks "stop it and quit? y/n" while Claude works or a command of
  its runs, a background job too; y or C-D again quits. Before, an idle
  Claude with a background job quit at once and went on alone, with no
  window, until the job ended and it had answered.
- Quitting stops Claude's commands along with it.

## 0.1.19 (2026-10-28)

- A `!` command's output shows only its last 12 lines in the answer
  pane, under a line that counts the rest. The terminal printed all of
  it while it ran, and Claude still gets all of it.

## 0.1.18 (2026-10-28)

- Hidden under gluon (another tab in front), deck drops its 1 s clock
  while Claude works, since its bar is not on screen. gluon now asks deck
  for its wait on every tab, so an idle Claude is paused there too.

## 0.1.17 (2026-10-28)

- `App::ask` sends a prompt from the app deck runs in, as if typed and
  sent with Enter; it waits in purple while Claude is busy. gluon's
  Ctrl+S uses it.

## 0.1.16 (2026-10-27)

- After 10 minutes with no turn, deck pauses Claude Code with SIGSTOP and
  lets it go on with the next prompt. An idle Claude Code woke about 25
  times a second (30 to 70 mW); a paused one costs nothing and stays
  listed in fleet. Not while Claude runs a job of its own.
  `DECK_PARK=<seconds>` sets the wait.

## 0.1.15 (2026-10-27)

- A message from another session, handed over by the fleet bus hook,
  shows as a prompt in the turn it came in, also after a restart.
  Claude Code now reports hook output to deck; deck reads only the
  fleet lines and skips the rest before parsing them.

## 0.1.14 (2026-10-27)

- The session's name in the top bar is purple (colour 13), as in fleet and
  in Claude Code's status line.

## 0.1.13 (2026-10-27)

- Under gluon, the line above the prompt is light green while deck has the
  keys, the colour gluon gives the frame of the box that has them.

## 0.1.12 (2026-10-27)

- The prompt is back on black under the thin grey line, with a black row
  between it and the status line.
- Under gluon's Home tab, deck draws in the bottom half of the screen
  (`set_area`), and tells gluon when a popup or the editor drew over the
  rest (`take_overdraw`).
- Answer text is white. It was light grey, and a line with a coloured word
  turned white after that word.
- The top bar has the same grey as the status line at the bottom.
- A long prompt breaks between words, so a word moves whole to the next
  row; only a word longer than a row is cut.

## 0.1.11 (2026-10-27)

- The prompt sits on the answer pane's colour under a thin line, instead
  of in a black strip of its own. It takes one row more.
- The tool log on the right shows every line of a prompt, not only the
  first, leaving out blank lines. A prompt longer than ten lines ends
  in "… N more lines".

## 0.1.10 (2026-10-27)

- Claude reads what a `!command` printed as soon as it ends, and answers
  it, so a script run in deck gets a reply. Before, the output waited for
  the next prompt. A reopened session shows such a turn as `! command`.
- "deck is old: restart" shows in amber at the next second of a turn, or
  the next key, after a rebuild. Before, it waited until the turn ended.
  The check is one readlink per wake, and it stops once deck is old.

## 0.1.9 (2026-10-27)

- C-Enter sends the purple prompts at once instead of after the turn. They
  turn blue when Claude reads them: at its next tool call, inside the turn
  it is on, or as the next turn if it was already writing its answer.
  deck asks Claude Code to echo each prompt it reads
  (`--replay-user-messages`), one short line per prompt.
- A reopened session shows such a prompt under the one it joined.
- The top line shows the step Claude is on and how long it has run, such
  as `Bash 1:05` or `thinking 0:31`, so a long quiet spell shows what it
  waits for. It costs nothing new: the line already redraws each second
  during a turn.
- Needs crust 0.1.69, which reads Ctrl+Enter as C-ENTER.

## 0.1.8 (2026-10-27)

- Up and Down reach the prompts of earlier runs in this folder, from deck
  and from Claude Code alike. deck now writes its prompts to Claude Code's
  `~/.claude/history.jsonl`, so they show up there too.
- `!command` runs the command in a shell, in the session's folder, with
  the terminal handed over. Claude gets what it printed with the next
  prompt, as in Claude Code.
- Up then Down brings back what you had typed but not sent.
- The answer pane follows its end, as a terminal does, so the answer before
  stays in view under a new prompt. Scrolled up to read, it stays put.
  Before, each new prompt jumped to the top of the pane.
- A reopened session shows its last ten turns, not only the last one. deck
  reads up to 32 MB from the end of the transcript for them, 1 to 63 ms on
  the biggest transcripts here, and parses only prompt and answer lines.
- Opening a session read its whole transcript, 773 MB for one, to find the
  folder it started in. deck now reads the first 200 KB; that open went
  from 479 to 52 ms.
- C-D quits at once. deck waited for Claude Code to end, 0.7 s when idle and
  up to 3 s in a turn. Now Claude Code ends on its own after deck has gone,
  in its own process group so closing the window does not cut it off. A
  turn still running is stopped, as before.
- C-D during a turn first says "Claude is working: C-D again stops the turn
  and quits", since quitting cuts the turn. A reopened session marks a turn
  that was cut with "✗ stopped"; before, its prompt stood with no answer
  and the session looked stuck.
- A session reopened after Claude Code compacted it showed the whole summary
  as the last prompt, in blue. deck now shows the prompt you typed.
- A prompt sent while Claude works shows at once under the running turn,
  in purple until deck hands it to Claude. Before, only the count in the
  top line showed it.
- "deck v… is old: restart" shows in bold amber, as in kastrup and tock.
  Before, it was dim grey like the version beside it.
- C-T also shows the last turn's tool calls in full, as C-O does. Under
  gluon, where deck is the Claude tab, C-O moves to the next tab.
- deck is a library too (`src/app.rs`), so gluon can run it as a tab.

## 0.1.7 (2026-10-26)

- With the HyperList drawing on, a table in an answer becomes HyperList
  Items, as in Claude Code. deck drew its lines first, and the hook then
  wrapped those lines as Items, so a table came out torn.

## 0.1.6 (2026-10-26)

- Claude's multiple-choice questions open as a menu, one per question.
  "Type an answer" takes free text; with several choices allowed, Enter
  ticks and "Done" sends. Esc answers none of them.
- When a turn ends, or Claude asks something, while another window is in
  front, a box shows (notify-send, drawn by herald). One call per turn,
  nothing while idle.
- C-F finds text in the answers and brings its row to the top; C-F then
  Enter finds the next.
- Shift+Enter starts a new line, as in Claude Code (needs crust 0.1.68).

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
