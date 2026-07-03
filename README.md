# azalea-reflection-proxy

Rust/azalea port of the architecture from
[aesthetic0001/mineflayer-reflection-proxy](https://github.com/aesthetic0001/mineflayer-reflection-proxy):
"replay mod, but live." The proxy owns the single real Microsoft-authed
connection to the target server; the bot (and later, vanilla-client viewers)
connect to the proxy locally. Whoever holds control drives the session;
everyone else watches.

## Why the bot connects THROUGH the proxy

This is the original's key design move and the reason spectating works at
all: since every controller's movement arrives at the proxy as serverbound
packets, the proxy always knows where the controlled player is and can
replicate that to viewers. It also makes control handoff nearly free —
it's just changing whose serverbound packets get forwarded.

Bot-side change: `Account::offline("reflected")` → `127.0.0.1:25566`
instead of `Account::microsoft(...)` → `hypixel.net`. The proxy does the
Microsoft auth now (same azalea-auth token cache).

## Usage as a library (the intended way)

Add the crate to your bot (path dependency until published):

```toml
[dependencies]
azalea-reflection-proxy = { path = "../azalea-reflection-proxy" }
```

Then spawn the proxy in-process and point your azalea bot at it — two
changed lines relative to a normal bot:

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
Custom frame plugins implement `ProxyPlugin` and register with
`.plugin(...)`. The standalone binary (`cargo run`, env-var config)
still exists as a thin wrapper over the same builder.

## Honest state

All four phases are implemented and compile; phases 1-2 (passthrough +
replicator basics) have been tested live. Phases 3-4 (world snapshot,
spectator fix, control handoff, controllerless stand-in) are untested
as of 2026-07-02.

### Commands (type in chat from any connected client)

- `,acquire` — take control of the session (steals it if someone else
  has it; they become a spectator)
- `,release` — give up control; the proxy answers keepalives and
  teleport confirms itself so the session stays alive with nobody
  driving (unless `always_first_control` is on, in which case the
  oldest viewer inherits control)
- `,spectate [username]` — lock the camera to a player entity (no arg
  = the reflected bot; repeat with no arg to detach). Viewers only.
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

### What is NOT ported (and why)

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

- Plugin pipeline (`plugin.rs`) — Forward / Drop / Replace verdicts on
  raw frames, mirroring the original's plugin order semantics. Wired but
  no plugin exists yet, so Drop/Replace paths are unexercised.
- Frame adapters (`relay.rs`) — FrameSource/FrameSink over azalea's raw
  connection halves. Done.
- Login legs (`upstream.rs`, `local_server.rs`) — full Microsoft auth +
  encryption + compression dance upstream; offline-mode mirror locally
  (uncompressed, loopback only). Done, live-tested.
- Session actor (`session.rs`) — one task owns the session: clientbound
  broadcast to all clients, serverbound forwarded only from the
  controller, viewers' acks/keepalives swallowed. Viewers joining
  mid-session get a join replay: cached config frames + synthesized
  FinishConfiguration, then Login, Respawn (if the session changed
  dimension), position, Game Event 13, and the full raw chunk cache —
  chunks turned out to be a hard join requirement, since the vanilla
  client won't leave "Loading terrain..." until the chunk under its
  feet loads. Entities/inventory/health are still phase 3, so a fresh
  viewer sees terrain but no mobs/items until live traffic fills in.
  Controller leaving tears the session down (handoff is phase 4).
- Spectator viewers (`reflect.rs`) — viewers get the full spectator
  kit on join (own-uuid player info + game event + abilities; modern
  clients key game mode off the player-info entry, so the event alone
  is not enough) and see the bot as a synthesized player entity,
  mirrored live from the controller's serverbound movement packets
  (the reason the bot routes through the proxy at all). The kit is
  re-asserted after every Login/Respawn broadcast, which would
  otherwise reset it. The session's own teleports, abilities, and
  game-mode changes are filtered away from viewers so their free
  camera survives; one position frame is let through after dimension
  changes so they land in the new world.
- World snapshot (`snapshot.rs`) — the snapshot.js port: entities
  (with positions accumulated from relative moves), tab list (merged
  player-info entries), scoreboards/teams, player inventory, health/
  food/xp, time, held slot and weather, all cached as raw frames and
  replayed to joining viewers after the chunks.
- Synchronization (`session.rs` + `reflect.rs`) — `,acquire` and
  `,release`/`,spectate` chat commands port synchronization.js: the
  acquiring client gets the real game mode + abilities back, the ghost
  bot entity removed, and a teleport onto the bot's pose (its accept
  is swallowed); the demoted client gets the spectator kit and the
  ghost back. With no controller the proxy stands in, answering
  keepalives and confirming teleports, so the session survives the
  bot disconnecting entirely.
- Packet ids (`ids.rs`) — the few ids the proxy matches on, pinned by
  `cargo test` against azalea's own packet types where construction is
  cheap; Login has a runtime first-game-frame guard instead.

## Why frames, not bytes, not typed packets

Byte forwarding is impossible: the upstream leg is encrypted with keys the
proxy negotiated. Fully-typed forwarding is unnecessary and fragile: the
proxy shouldn't break because it can't parse a packet it doesn't care
about. Raw frames (packet id + body after decrypt/decompress) are the
middle ground — phase 1 interprets nothing; later plugins parse only the
frames they need.

## Phase roadmap (mirrors the original's plugin list)

1. **Passthrough** — done, live-tested: one bot, faithful relay.
2. **Replicator** — implemented, untested: extra viewer connections
   attach to the running session; clientbound broadcast to all;
   serverbound forwarded only from the control-holder; viewers'
   keepalive replies and teleport confirms swallowed at the proxy.
3. **Snapshot**: cache login/registry/chunks/entities/inventory/health so
   a viewer can join mid-session and be synced to current state. Biggest
   single work item.
4. **Control handoff + reflected entity + physics sync**: `,acquire` /
   `,release` chat commands, synthesize a player entity representing the
   controlled character for non-controllers, align positions on handoff
   (azalea-physics-backed simulation, the GrimAC-style option from the
   original's README).

## Security note

The local leg is offline-mode by design. Never bind it beyond loopback:
anyone who can reach the port can drive the authenticated session.

## Anticheat honesty

The original's "anticheat compliance" claim was for THEIR proxy + THEIR
configured mineflayer at protocol 1.8.9. None of that transfers. This
build revalidates that property from zero, and handoff timing artifacts
are exactly the kind of thing movement checks notice. Test on something
disposable first.
