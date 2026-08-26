# Discord Rich Presence — feasibility note, and what was built from it

**Status: decided and built, 2026-08-26. One human-only step outstanding.**
The body below is the original assessment of 2026-08-22, kept as written
because its protocol notes and its costing are what the build was made
against. What changed is at the top and the bottom: [Decided](#decided) states
the calls, and [What was built](#what-was-built) states what exists and what
still has to be done by hand in Discord's developer portal.

## Decided

Both open decisions at the bottom of this note were settled by the project
owner on 2026-08-26:

1. **Static, not game-aware.** No foreground-process detection. The card says
   the same thing whatever is running.
2. **Yes, the application is being registered.** The id is the outstanding
   step.

Two further calls, which this note had not asked but the build needed:

3. **Live whenever the daemon is running**, not only while armed. The timer is
   therefore "how long Trix has been up", not "how long capture has been
   armed".
4. **On by default** (`discord_presence = true`), against this note's own
   recommendation. The reasoning that overrode it: a presence is visible the
   instant it happens and reversible in two clicks, so its mistake is nothing
   like the library ceiling's silent, permanent one. `crates/trix-core/src/config.rs`
   carries that argument beside the field.

The original assessment follows unchanged.

## What the feature is

Medal publishes a Discord presence while it runs, so a user's friends list
shows what they are playing and that Medal is capturing it. The equivalent for
Trix would be a line like "Clipping with Trix" while armed.

## Verified facts about this codebase (checked 2026-08-22, v0.6.0)

- Trix already speaks named pipes on both ends: `crates/trix-daemon/src/pipe.rs`
  (732 lines, server) and `crates/trix-ui/src/pipe.rs` + `pipe_reader.rs`
  (client, with request/response correlation). The Discord client is *simpler*
  than either — no correlation, no reader thread strictly required.
- `Config` in `crates/trix-core/src/config.rs` is `#[serde(default,
  deny_unknown_fields)]`, so adding a `discord_presence: bool` is a one-liner
  plus a default. Old config files stay loadable.
- The Settings row pattern to copy is the clips-folder row shipped in 0.6.0
  (`crates/trix-ui/web/src/views/Settings.svelte`).
- Arm / disarm / clip already broadcast events from
  `crates/trix-daemon/src/state.rs`; presence updates hang off those same
  transitions rather than needing new plumbing.
- **There is no foreground-process detection anywhere in the tree.**
  `GetForegroundWindow`, `GetWindowThreadProcessId`, and
  `QueryFullProcessImageName` all appear zero times. Trix captures a monitor by
  index (`Config::monitor_index`), so it does not know what game is running.
  `crates/trix-daemon/src/window.rs` is the hidden message-only window for the
  tray and the hotkey — unrelated.

## The protocol, in enough detail to build it

Discord RPC is **local IPC, not a network API**. Trix would open no new
outbound connections; Discord's own client does all the talking. That matters
for this project's "no telemetry, no account" posture.

- Pipe: `\.\pipe\discord-ipc-0` through `-9`. Try them in order; the first
  that opens is Discord. None open = Discord is not running = no-op and retry
  later.
- Framing: 4-byte little-endian opcode, 4-byte little-endian payload length,
  then UTF-8 JSON.
- Opcodes: 0 = HANDSHAKE, 1 = FRAME, 2 = CLOSE, 3 = PING, 4 = PONG.
- Handshake payload: `{"v": 1, "client_id": "<app id>"}`. Discord answers with
  a READY dispatch.
- Then send op 1 frames: `{"cmd": "SET_ACTIVITY", "nonce": "<uuid>", "args":
  {"pid": <daemon pid>, "activity": {"details": "...", "state": "...",
  "timestamps": {"start": <unix secs>}, "assets": {"large_image": "<asset
  key>", "large_text": "..."}}}}`.
- Drain replies even if they are ignored, or the pipe backs up.

**Dependency call:** there is a `discord-rich-presence` crate, but we already
own working pipe code and the framing is eight bytes. Hand-rolling ~200 lines
in a new `crates/trix-daemon/src/presence.rs` matches this codebase's taste
(direct `windows` crate, minimal dep tree) better than adding a crate.

## The three real costs

### 1. A Discord application must be registered (blocking, human-only)

Someone with the Discord account has to create an application in the developer
portal, name it "Trix", and upload the logo as a Rich Presence art asset. About
15 minutes, once. Consequences to accept up front:

- The client ID gets baked into the shipped binary. That is fine — it is public
  by design, not a secret.
- Every shipped copy is permanently tied to that one application. Delete the
  app and every installed Trix silently shows nothing.

An agent cannot do this step. It needs the account.

### 2. "Playing <game>" is a different, bigger feature

Discord renders the *application's* name as the header, so the static version
reads:

```
Playing Trix          <- the Discord app name, not ours to word
Clipping with Trix    <- details
Ready to clip         <- state
```

The header verb is Discord's; expect "Playing Trix" and confirm what the portal
allows when registering the app.

That top line is redundant, and the redundancy is the whole gap versus Medal.
Medal's presence is interesting because it names the **game**. Trix cannot,
today — see the verified facts above. Closing that gap means a foreground-window
poll, a process-name lookup, and a filter that refuses to announce
"explorer.exe" or, worse, a window title. Roughly another day, and it is the
part that can feel *wrong* rather than merely absent when it misfires.

### 3. Rate limit and standing cost

- Discord throttles `SET_ACTIVITY` to roughly **one update per 15 seconds**.
  A per-clip "Clip saved!" flash is not reliable. Presence must be coarse
  (idle / armed / recording) and coalesced.
- One more permanent thread, a persistent pipe handle, and a reconnect timer,
  in a daemon whose selling point is a 9.4 MB working set. Small, not zero.

## Two versions

| Version | Effort | Assessment |
|---|---|---|
| Static "Clipping with Trix" | ~half a day + the 15-minute portal chore | Marginal. Mostly free advertising for Trix, not a user feature. |
| Game-aware, like Medal | ~2 days total | The one people actually like. Needs foreground detection first. |

**Recommended default: OFF.** Presence broadcasts to a whole friends list that
the user is running a clip recorder. Trix's audience skews toward people who
picked it *because* it does not phone anywhere. Medal defaults it on because
visibility is Medal's business model; it is not ours.

## Open decisions (settle these before writing a spec)

1. **Static or game-aware?** They are different features. Only the second is
   worth the pipe, but it drags in foreground-process detection, which is new
   surface area and a new class of "it said the wrong thing" bug.
2. **Is the Discord application going to be registered at all?** Everything
   else is blocked on it. No app, no feature.

## Files a build would touch

- Create: `crates/trix-daemon/src/presence.rs` (~200 lines), registered in
  `crates/trix-daemon/src/lib.rs`
- Modify: `crates/trix-core/src/config.rs` (one field + default)
- Modify: `crates/trix-daemon/src/state.rs` (fire updates on the existing
  arm / disarm / clip transitions)
- Modify: `crates/trix-ui/web/src/views/Settings.svelte` (toggle row)
- Game-aware version only: foreground detection, probably its own module

---

## What was built

Built 2026-08-26 on `feat/discord-presence`.

### The card

```
Playing                            <- Discord's own label
Clipping with Trix                 <- the application's name in the portal
Without thinking FPS and Memory    <- activity `details`
[logo] 3:48                        <- `assets.large_image` + `timestamps.start`
[ Get Trix ]                       <- `buttons[0]`, to /releases/latest
```

**This corrects §2 above.** That section assumed the header would read
"Playing Trix" with "Clipping with Trix" underneath as `details`. The bold line
is the *application's* name, and Discord ignores a `name` sent in the activity
— so the requested wording is obtained by **naming the application "Clipping
with Trix"**, and `details` carries the tagline instead. There is no `state`
line; the card is two lines, art, clock and one button.

### Files

- **Created** `crates/trix-daemon/src/presence.rs` — framing, handshake,
  `SET_ACTIVITY`, the reconnect loop, and 10 tests. Hand-rolled as recommended;
  no new dependency. `Session` is a trait so the state machine is testable
  without a running Discord.
- **Modified** `crates/trix-daemon/src/lib.rs` — `pub mod presence;`
- **Modified** `crates/trix-core/src/config.rs` — `discord_presence: bool`,
  `#[serde(default = "default_true")]`
- **Modified** `crates/trix-daemon/src/state.rs` — `Daemon::discord_presence_enabled`
- **Modified** `crates/trix-daemon/src/main.rs` — spawns the thread beside the
  stats thread
- **Modified** `crates/trix-ui/web/src/lib/settings.ts` (+ its test) — the
  toggle row, Settings → Trix. `Settings.svelte` needed no change: it iterates
  `FIELDS`, and `bool` was already a kind it renders.

`state.rs`'s arm/disarm transitions are **not** used, contrary to the plan in
"Files a build would touch". Presence follows "the daemon is up", so there is
no transition to hang it off; the thread polls `discord_presence` every 250 ms
and that poll is the entire wiring between the settings toggle and the card.

### Still to do by hand — nobody else can

`presence::CLIENT_ID` is `""`, and while it is empty the thread is never
spawned at all (logged once at startup). To finish:

1. Create an application at <https://discord.com/developers/applications>.
2. **Name it exactly `Clipping with Trix`** — this is the bold line of the
   card, and it is the only way to set it.
3. Rich Presence → Art Assets → upload `assets/logo.png` **under the key
   `logo`**. Set it as the application icon too, as a fallback.
4. Copy the Application ID into `presence::CLIENT_ID`.

Nothing else is outstanding. Rebuilding after step 4 is the whole deployment.
