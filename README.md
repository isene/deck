# deck

![Rust](https://img.shields.io/badge/language-Rust-orange) ![Unlicense](https://img.shields.io/badge/license-Unlicense-green) [![Fe2O3](https://img.shields.io/badge/suite-Fe%E2%82%82O%E2%82%83-b7410e)](https://github.com/isene/fe2o3)

A paned terminal front for Claude Code. Part of the
[Fe2O3](https://github.com/isene/fe2o3) Rust terminal suite.

```
 deck  #myproject  ● working 0:12 · 3 tools · Bash 0:04   Esc stops     PgUp/PgDn scroll · ^O tools · ^D quit
› Why does the build fail on ARM?                  › Why does the build fail on ARM?
                                                   ✓ Bash Build for ARM
The linker flag is set for x86 only.               ✓ Read ~/src/build.rs
...                                                ↻ rewrite: answer:1: 31 words (max 22)
› _
user@host ~/src/myproject [Opus 5.5 high] [12%] ...
```

- The answer gets the big pane. New text comes in at the bottom, as in a
  terminal, and PgUp/PgDn scroll back. While you read further up, the
  pane stays where you are. A reopened session shows its last ten turns.
- Each tool call is one line in the log on the right: ✓ when it worked,
  ✗ when it failed. C-O shows the last turn's calls with their output.
- When a stop hook sends an answer back, you see only the rewrite, and one
  log line says what the hook found.
- The bottom row runs the `statusLine` command from Claude Code's
  settings, after each turn, with deck's version at the right.
- The answer pane takes the session's background from fleet's
  `~/.fleetrc` (`session <tag> <workspace> <colour>`). C-B picks a new
  one in `prism` and keeps it there.
- The prompt grows to as many rows as it needs, up to a third of the
  screen. Shift+Enter, C-J, or `\` then Enter starts a new line.
- Enter on a closed session in fleet opens it in deck.
- A message from another session (fleet's bus) shows as a prompt in the
  turn it came in, as if you had typed it.
- When Claude asks a multiple-choice question, it opens as a menu. Esc
  closes it, and Claude is told the question went unanswered.
- When a turn ends, or Claude asks something, while another window is in
  front, a box shows at the top right (`notify-send`, which herald draws).

## Use

```
deck              start a new session in this directory
deck <tag>        open the session with this bookmark tag (from cc-sessions)
deck <id>         open the session with this id
```

| Key | Does |
|---|---|
| Enter | send; while Claude works, the message waits its turn, in purple |
| C-Enter | send the purple messages now; they turn blue when Claude reads them |
| S-Enter, C-J, \\ Enter | a new line in the prompt |
| Up, Down | the row above or below, else earlier prompts, also from past runs |
| !command | run it in a shell; Claude reads what it printed and answers |
| Esc | stop the turn Claude is on |
| PgUp, PgDn | scroll the answers |
| C-F | find in the answers; C-F then Enter finds the next |
| C-PgUp, C-PgDn | scroll the tool log |
| C-O | the last turn's tool calls in full, and back |
| C-G | edit the prompt in `$EDITOR` (scribe if unset) |
| C-Y | copy the last answer, as drawn |
| C-S-Y | copy what the answer pane shows, scrolled or not |
| C-W, C-S-W | answer pane a tenth wider, narrower |
| C-B | pick the answer pane's colour in `prism` |
| C-? | the keys in a popup |
| y, n | answer a permission question |
| C-L | draw the screen again |
| C-D | quit; during a turn, press it twice |

deck refuses a session that is open in Claude Code already. For what deck
lacks (slash command dialogs, plan mode, questions with choices), quit and
run `claude --resume <id>` on the same session.

## How it works

deck runs `claude -p --input-format stream-json --output-format
stream-json --permission-prompts host` and reads its events. The answer
pane shows each turn's `result`, which is the final text after any stop
hook rewrite. When `~/.claude/hooks/hyperlist-display-asm` is installed
and switched on, answers are drawn through it, as Claude Code does.

Idle, deck uses no CPU: it waits on the keyboard, and the thread that reads
Claude's output wakes it with a signal. While Claude works, the bar's clock
wakes it once a second.

Claude Code itself wakes about 25 times a second while idle, 30 to 70 mW.
So after 10 minutes with no turn, deck pauses it (SIGSTOP), and the next
prompt lets it go on (SIGCONT), with nothing to reload. A paused session
still shows as open in fleet. While Claude runs a job of its own (a
background command, a monitor), it is not paused, so the job's end can
wake it. `DECK_PARK=<seconds>` sets the wait.

## Build

```
git clone https://github.com/isene/crust
git clone https://github.com/isene/deck
cd deck && cargo build --release
```

## License

[Unlicense](https://unlicense.org): public domain.
