# azalea-reflection-proxy

Rust/azalea port of
[aesthetic0001/mineflayer-reflection-proxy](https://github.com/aesthetic0001/mineflayer-reflection-proxy):
"replay mod, but live." The proxy owns the single real Microsoft-authed
connection to the target server; your bot and any vanilla-client viewers
connect to the proxy locally. Whoever holds control drives the session;
everyone else spectates — and control can move between clients at any
time with a chat command.

## Why the bot connects THROUGH the proxy

This is the original's key design move and the reason spectating works at
all: since every controller's movement arrives at the proxy as serverbound
packets, the proxy always knows where the controlled player is and can
replicate that to viewers. It also makes control handoff nearly free —
it's just changing whose serverbound packets get forwarded.

Bot-side change: `Account::offline("reflected")` → `127.0.0.1:25566`
instead of `Account::microsoft(...)` → `hypixel.net`. The proxy does the
Microsoft auth now (same azalea-auth token cache, so login is interactive
at most once per account).

## Usage

Add the crate to your bot:

```toml
[dependencies]
azalea-reflection-proxy = "0.1"
```

Spawn the proxy in-process and point your azalea bot at it — two changed
lines relative to a normal bot:

```rust
use azalea_reflection_proxy::ReflectionProxy;

let proxy = ReflectionProxy::builder()
    .target("mc.hypixel.net")
    .email("account@example.com")   // proxy owns the Microsoft auth now
    .spawn()
    .await?;

ClientBuilder::new()
    .set_handler(handle)
    .start(Account::offline("reflected"), proxy.local_addr())  // was: microsoft + real host
    .await?;
```

Spectate by adding a vanilla-client server entry for the same address
(default `127.0.0.1:25566`; `.bind("127.0.0.1:0")` picks a free port).
The client must be on the same protocol version as the azalea release
this crate builds against. A standalone binary (`cargo run`, configured
via `PROXY_EMAIL` / `PROXY_TARGET` / `PROXY_BIND` / `PROXY_AUTH_CACHE`
env vars) wraps the same builder.

### Commands (type in chat from any connected client)

- `,acquire` — take control of the session (steals it if someone else
  has it; they become a spectator)
- `,release` — give up control; the proxy answers keepalives and
  teleport confirms itself so the session stays alive with nobody
  driving (unless `always_first_control` is on, in which case the
  oldest viewer inherits control)
- `,spectate [username]` — lock the camera to a player entity **and**
  show the bot's HUD (inventory, held item, health/hunger, xp) — the
  same on-screen UI `,acquire` gives you, without taking control. No
  arg = the reflected bot; repeat with no arg to drop back to a
  free-flying spectator. Viewers only.
- `,gamemode <survival|creative|adventure|spectator|0-3>` —
  client-side game mode for the issuing viewer

### Builder options beyond the basics

- `.whitelist(["Name", ...])` — only these usernames may connect
  (case-insensitive); everyone else gets a disconnect message
- `.max_clients(n)` — cap simultaneous clients
- `.always_first_control(true)` — original's `alwaysFirstControl`
- `.plugin(Box::new(...))` — frame-level ProxyPlugin pipeline

### Events (port of the original's server events)

```rust
let mut events = proxy.subscribe();
tokio::spawn(async move {
    while let Ok(ev) = events.recv().await {
        // ProxyEvent::{SessionStarted, SessionEnded, ClientJoined,
        //              ClientLeft, ControlChanged}
        println!("{ev:?}");
    }
});
```

## How it works (module map)

- Session actor (`session.rs`) — one task owns each session: clientbound
  traffic broadcast to every client, serverbound forwarded only from the
  controller, viewers' acks/keepalives swallowed. Handles the `,command`
  set, control handoff (the acquiring client gets the real game mode +
  abilities back, the ghost bot entity removed, and a teleport onto the
  bot's pose with its accept swallowed), and the controllerless
  stand-in: with nobody driving, the proxy answers keepalives and
  confirms teleports itself, so the session survives the bot process
  exiting entirely.
- Join replay (`session.rs` + `snapshot.rs`) — viewers joining
  mid-session get cached config frames + a synthesized
  FinishConfiguration, then Login, Respawn (if the session changed
  dimension), position, Game Event 13, the full raw chunk cache, and
  the world snapshot. Chunk replay is a hard requirement: the vanilla
  client won't leave "Loading terrain..." until the chunk under its
  feet loads.
- World snapshot (`snapshot.rs`) — the snapshot.js port: entities (with
  positions accumulated from relative moves), tab list (merged
  player-info entries), scoreboards/teams, player inventory (both the
  container-0 packets and the 1.21.2+ per-slot `set_player_inventory`),
  health/food/xp, time, held slot, weather, and boss bars (accumulated
  per uuid and replayed as a synthesized `Add`, so a viewer joining
  mid-fight doesn't crash on the next boss-bar update). Cached as raw
  frames and replayed to mid-session joiners.
- Spectator viewers (`reflect.rs`) — viewers get the full spectator kit
  on join (own-uuid player info + game event + abilities — modern
  clients key game mode off the player-info entry, so the event alone
  is not enough), re-asserted after every Login/Respawn broadcast. They
  see the bot as a synthesized player entity mirrored live from the
  controller's serverbound movement packets. The session's own
  teleports, abilities, and game-mode changes are filtered away from
  viewers so their free camera survives; one position frame is let
  through after dimension changes so they land in the new world.
- Login legs (`upstream.rs`, `local_server.rs`) — full Microsoft auth +
  encryption + compression dance upstream; offline-mode mirror locally
  (uncompressed, loopback only).
- Plugin pipeline (`plugin.rs`) — Forward / Drop / Replace verdicts on
  raw frames, in registration order, mirroring the original's plugin
  semantics (`onReadReal` ≈ `on_clientbound`, `onWriteReal` ≈
  `on_serverbound`, `bindToReflected` ≈ `on_session_start`).
- Packet ids (`ids.rs`) — every id the proxy matches on, pinned by
  `cargo test` against azalea's own encoders where construction is
  cheap; Login has a runtime first-game-frame guard instead. Includes a
  canary test for an azalea 0.16 bug (its `player_info_update` writer
  omits `update_list_order`/`update_hat` entry data) so a fixed azalea
  release announces itself as a test failure.

## Why frames, not bytes, not typed packets

Byte forwarding is impossible: the upstream leg is encrypted with keys
the proxy negotiated. Fully-typed forwarding is unnecessary and fragile:
the proxy shouldn't break because it can't parse a packet it doesn't
care about. Raw frames (packet id + body after decrypt/decompress) are
the middle ground — the relay interprets nothing, and only the code
that needs a specific packet (snapshot, reflected entity, commands)
parses that one packet.

## Status

Everything above is implemented. The passthrough and replicator paths
(bot through proxy, viewer join, terrain), spectator mode, and
`,spectate` with the bot's HUD have been tested live — hardening the
mid-session join (modern inventory sync, boss-bar replay) came directly
out of that testing. Control handoff, the event stream, and
whitelist/max_clients have unit-test coverage but little live mileage
yet. Treat `,acquire` with care on anticheat-guarded servers: position
is aligned on handoff, but momentum is not carried over.

## What is NOT ported (and why)

- `,connect` + the limbo world (`noLimbo`, `spawnPosition`) — clients
  here replicate immediately on join, which is exactly what the
  original's `createBotReflected` integration mode forces
  (`noLimbo: true`); a limbo lobby only matters for its standalone
  public-server mode.
- `version` option — the protocol version is pinned by the azalea
  release this crate builds against.
- Physics simulation while uncontrolled — the original hosts a
  mineflayer bot in-process and re-enables its physics; here the bot
  is your own azalea process, so the proxy stands in (keepalives +
  teleport confirms) and the player idles server-side instead.
- `spoofUsername`/anonymize — would require rewriting every forwarded
  player-info/chat frame; omitted.
- The explosion-velocity handoff trick from synchronization.js —
  handoff here aligns position with a plain teleport (the GrimAC-style
  option from the original's README); momentum is not carried over.
- Skin textures on the reflected bot entity (needs a signed
  sessionserver profile lookup); it renders with a default skin.

## Security note

The local leg is offline-mode by design. Never bind it beyond loopback:
anyone who can reach the port can drive the authenticated session. If
you must share it, `.whitelist(...)` and `.max_clients(...)` reduce the
blast radius — they do not make it safe on the open internet.
