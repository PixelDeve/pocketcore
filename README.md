# pocketcore

Ultra-light MCPE 0.14.3 (protocol 70, RakNet v7) minigame server core in Rust
with embedded Luau plugins.

    cargo test            # codec, RakNet, world, combat and inventory tests
    cargo run --release   # listens on 0.0.0.0:19132 (server.toml)
    RUST_LOG=debug cargo run

Stages: 1 layout, 2 RakNet v7 + reliability, 3 login / chunks, 4 gameplay +
Luau plugins (this release).

## Commands

In-game chat (`/op Steve`) and the server console (`op Steve`, no slash needed):

    /help [command]                          everyone
    /op <player>   /deop <player>            operators
    /kick <player> [reason]
    /ban <player> [reason]   /unban <player> works while the player is offline
    /mute <player>  /unmute <player>         blocks chat only
    /list                                     everyone
    /say <message>                           operators, server-wide announcement
    /heal [player]                           operators, restore health (self in-game)
    /clear <player>                           operators, clear inventory
    /give <player> <item-id> [count] [meta]   operators, give an item
    /tp <player> | <player> <player> | <x> <y> <z> | <player> <x> <y> <z>   (~ = relative)
    /tpall <player>                            teleport all online human players to a player
    /fill <x1> <y1> <z1> <x2> <y2> <z2> <block[:meta]> [meta]   (max 65536 blocks, y 0-127)

`/fill` takes a name (`stone`, `wool`, `glass`, `air` ...) or a numeric id, e.g.
`/fill ~ 3 ~ ~10 3 ~10 wool 14`. Every command except `/help` needs operator
status. The console always has it, so create the first op there: `op YourName`
(or put the name into `ops.txt`). Lists are saved in `ops.txt`, `banned.txt` and
`muted.txt` (one lower-case name per line). Plugins still see chat first: a
plugin that returns `true` from `on_player_chat` handles the command itself.

## Bedwars (4 teams: red, green, yellow, blue)

Built in (`src/bedwars/`); the old Luau prototype lives in `plugins/examples/`.

**Template map.** Put your world at `maps/original/` (an MCPE 0.11-0.16 world folder
with `db/` + `level.dat`) or a ready `maps/original.pcw`. On first start the folder is
imported and cached as `maps/original.pcw`; with neither, an empty generated world is
used so you can build one. Every joining player is placed in a private copy-on-write
clone of that template (a running match never blocks joiners; a new clone is made at once).
Clones are deleted after the match. Time is always 6000 in every world.

**Setup (operators, standing in the original map: `/visit original` or `/map original`).**
Everything is saved to `maps/original.toml` (+ `maps/original.pcw` on `/bedwars save`
and autosave) and inherited by all clones.

    /waitingroom create | delete
    /team create <red|green|yellow|blue>     saves your position and facing
    /spawner basic                           iron + gold generator at your feet
    /spawner advance                         emerald generator
    /spawner remove                          removes the nearest one
    /shop npc | remove                       shop NPC (tap it to open the shop)
    /bed team <color>                        gives a bed; place it, it registers
    /bedwars status | reload | save          what is still missing / reload shop.toml / save now
    /lobby   /stats                          everyone

A match only starts once the waiting room, all 4 team spawns and all 4 beds are set.

**Match.** Humans wait in the waiting room and are spread over the 4 teams. If the
lobby is not full after `bot_wait_secs` (40), bots fill the free slots, then a
countdown starts. Bots stay on their island, attack only enemies, never teammates.
Respawn while the bed stands; eliminated for good without it. Only player-placed
blocks and enemy beds can be broken (normal players only; the original map can only be edited
by operators); a player can hurt every enemy but never a teammate; falling below `void_y` kills. Bed
destruction is announced with a sound; the last team standing wins, gets coins
(`stats.toml`) and everyone returns to a fresh lobby.

**Shop.** `shop.toml` (created on first start, hot-reloaded, no recompiling):
categories, items, amounts, costs in iron/gold/emerald. Tap the NPC: a chest GUI
opens (categories, then items; tap an item to buy, the arrow goes back). 0.14 slots
cannot show names or prices, so opening a category also prints the price list in
chat. Players can switch with `/shop chat` (chat shop: type the numbers) and back
with `/shop gui`; `shop = "chat"` in `[bedwars]` turns the GUI off server-wide.
GUI wire format (fake chest block + BlockEntityData, then ContainerOpen/SetContent,
window id 10) follows PocketMine-era fake-chest menus but is unverified on a real
client: if the window never opens, use `trace_packets` and the chat shop meanwhile.

**Gamemode.** `/gamemode` and `/gm` (operators): `0|s`, `1|c`, `2|a`, `3|sp`
(optionally a player name).

- *Creative*: the server fills the client's creative menu (`src/creative.rs`, edit the
  list there). Operators in creative may build and break anywhere, also inside a
  match world, and have a longer reach.
- *Practice mode*: while the map setup is incomplete (see the `map setup incomplete` line in the
  console) no match can start, so the waiting room plays like a match: survival, a sword and wool,
  build/break only player-placed blocks, damage (never teammates), fall damage and void death.
  Finish the setup (`/bedwars status`) and the normal lobby (adventure, no building) returns.
- *Auto jump*: switched on for every game mode (AdventureSettings bit 0x40 in the `legacy`
  table, 0x20 in `modern`), so players step up single blocks without jumping.
- *Creative operators*: the server trusts any block id they report, and a creative break is
  accepted from either RemoveBlock or PlayerAction START_BREAK. Every refused creative place or
  break is logged as `creative place/break ... refused: <reason>` (out of reach, no item in hand,
  clicked block is air or outside the loaded world, ...), so a stuck client can be diagnosed
  from the console.
- *Spectator*: shown to the client as creative (it only knows 0-2) plus the no-clip
  adventure flag; invisible and untouchable.
- *Fall damage*: survival players take `ceil(fallen - 3)` damage when they land.
  Water, lava, ladders, vines and cobwebs cancel the fall. Only while a match runs.
- `[protocol] adventure_flags = "legacy" | "modern"` picks the AdventureSettings bit
  table (0.14: allow flight 0x80 / no clip 0x100; 0.15+: 0x40 / 0x80). If flying or
  spectator no-clip does not work on your client, flip it.

Config: `[bedwars]` in `server.toml`.

## Plugin API (plugins/*.luau, hot-reloaded)

Events (define as global functions; return `true` to cancel):
`on_server_start()`, `on_tick(tick)`, `on_player_join(player)`,
`on_player_quit(player)`, `on_player_chat(player, msg)` (commands arrive here
as "/cmd ..."), `on_player_move(player, pos)`, `on_block_place(player, pos,
block_id, meta)`, `on_block_break(player, pos)`, `on_player_damage(victim,
attacker, damage)` (return `true` to cancel or a number to override),
`on_entity_interact(player, entity_id)`, `on_player_death(victim, killer)`,
`on_player_respawn(player)`.

player: `get_eid get_name get_position is_online get_health set_health
get_gamemode set_gamemode send_message send_popup send_tip teleport(x,y,z)
give_item(id,count,meta) clear_inventory set_armor(h,c,l,b) set_knockback(h,v)
set_spawn(x,y,z) kick(reason)`

world: `get_block set_block(x,y,z,id,meta) spawn_item(x,y,z,id,count,meta)
reset_arena get_spawn set_tracking commit_baseline`

server: `broadcast get_players get_player(name) get_player_count get_tick`

Globals: `log(msg)`. Plugins run sandboxed (no io/os/require) with a 50 ms
budget per handler call. Compare players by `get_eid()`, not by handle.

## Tuning against a real client

Wire details that could not be verified offline are configurable in
`server.toml` ([protocol]): `chunk_order_byte`, `start_game_tail`,
`hotbar_offset`, `join_extras`. Run with `trace_packets = true` and
`RUST_LOG=debug` to see what the client sends.

If a client sits on the loading screen, read the console: every join logs
`PlayStatus + StartGame sent`, `chunk radius request N`, `chunks sent, sending
PLAYER_SPAWN` and any packet id the server does not understand before spawn.
A client that never sends its chunk radius request is served after 2 seconds
anyway.

## Wire note (0.14.x)

Every game packet is wrapped in a `0x8e` byte inside the RakNet frame
(`0x8e`, packet id, body). `Outbox::send` adds it and `GameServer::on_packet`
strips it; the Login packet id is `0x8f`.


### V5 teleport reliability note

Far teleports defer the local-player movement packet until destination chunks
have streamed. If another teleport is requested while that stream is pending,
the newest destination is retained and is sent after the queue drains. See
`BLOCK_INTERACTION_DIAGNOSIS.md` for the regression scenario and verification
limitations.

### V6 chunk-stream scheduling note

When a player moves or teleports while chunk packets are still queued, the
server now discards pending chunks that are outside the new view's retention
area and makes those chunks eligible to be requested again later. This keeps
old-location chunk traffic from consuming the stream budget ahead of the new
destination. A regression test covers a far move while the original radius is
still queued. This is a source-level patch and has not been compiled or tested
against a live 0.14.3 client in this environment.


### V7 block-placement inventory consistency

A successful survival placement now consumes the matching item variant (item ID
+ damage) and immediately sends the updated inventory/equipment state to the
client. The client-reported stack is trusted only when that exact variant exists
in the server inventory; otherwise placement falls back to the server-held item.
A failed world write no longer consumes an item and resynchronizes both the block
and inventory. This addresses stale hotbar counts and metadata-variant stacks
being consumed incorrectly. Source-level change only; run `cargo test` and verify
with a 0.14.3 client before deployment.


### V8 protocol and synchronization repairs

- Protocol-70 chunk biome-color entries are emitted as ordered `biome ID, R, G, B`
  bytes. The previous little-endian packed integer reversed the channels, which
  could produce incorrect grass tint.
- The legacy chunk payload now ends after the 256 biome-color entries (83,200
  bytes); it no longer appends a later-format extra-data count.
- LevelEvent event IDs and event data use unsigned VarInts, matching the
  GenisysPro/PocketMine legacy packet encoder. This covers block-break particles
  and the sound events sent through LevelEvent.
- Bot movement and attack/mining animations are broadcast to all spawned viewers
  in the bot's world so distant teammates do not miss entity updates.
- Added operator command `/tpall <player>` for moving all other online, alive,
  spawned human players to the target, including cross-world chunk streaming.

These are source-level fixes. Verify chunk rendering, sounds, bot visibility and
`/tpall` with an actual MCPE 0.14.3 client before using the build publicly.


## V9 teleport and generator updates

- Real-player teleports wait until the destination chunk and its eight neighbouring chunks have been sent, following the `checkTeleportPosition()` pattern used by GenisysPro.
- Chunk requests queued for streaming are tracked separately from chunk payloads actually queued to RakNet. Teleport readiness uses the latter.
- The local player receives the correction using the local entity ID only. Other players receive the teleported entity update, never a duplicate self-entity update.
- Same-world observer updates are deferred until the destination terrain is ready. Cross-world switches clear the old world's chunk readiness state.
- Teleport acknowledgements use a one-block squared-distance tolerance. Stale pre-teleport movement is rejected and the server resends the correction.
- Bedwars resource drops now stop at 20 iron, 10 gold, and 4 emerald items near each corresponding generator.
- Emerald generators create an invisible floating name-tag hologram showing the seconds until the next emerald. Its text is updated in place once per second.
