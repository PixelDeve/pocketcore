# Block interaction diagnosis — corrected against the legacy 0.14 packet sources

## Confirmed protocol correction

The previous patch was still wrong. It inserted a `selectedSlot: i32` field into `UseItemPacket`, but the PocketMine legacy 0.14 `UseItemPacket::decode()` contains:

1. x, y, z as three big-endian 32-bit integers
2. face as one byte
3. click offset as three big-endian floats
4. player position as three big-endian floats
5. legacy 0.14 item slot

There is no selected-slot field and no block-ID varint in this protocol generation. The prior decoder therefore consumed the first four bytes of the item slot as a selected slot and then tried to decode a second slot, causing item decode/trailing-data failure. That prevents normal placement from reaching validation/set_block.

The legacy `RemoveBlockPacket` layout is entity ID (8-byte big-endian), x (i32 BE), z (i32 BE), y (u8). The existing decoder's `block_pos_xzy()` matches this. The legacy `PlayerActionPacket` layout is entity ID (8-byte big-endian), action (i32 BE), x/y/z (i32 BE), face (i32 BE); the existing decoder matches this.

The legacy PocketMine `AdventureSettingsPacket` encoder writes only one big-endian u32 flags field. The previous project emitted an additional permission u32, which has been removed for 0.14 compatibility.

The legacy item slot NBT length is read with PocketMine `getShort()` (big-endian), so the Rust slot decoder/writer has been aligned to big-endian.

## Sources used

- PrismarineMC/Prismarine-0.12 legacy PocketMine `UseItemPacket.php`: exact decode field order.
- PrismarineMC/Prismarine-0.12 legacy PocketMine `RemoveBlockPacket.php`: exact coordinate order.
- PrismarineMC/Prismarine-0.12 legacy PocketMine `PlayerActionPacket.php`: exact action layout.
- PrismarineMC/Prismarine-0.12 legacy PocketMine `AdventureSettingsPacket.php`: flags-only packet.
- PrismarineMC/Prismarine-0.12 legacy `BinaryStream.php`: item slot and NBT length encoding.

## Remaining runtime verification

Compilation and live MCPE 0.14.3 testing still must be performed in an environment with Rust/Cargo and a real 0.14.3 client. Do not treat source-level decoder tests as proof of successful live gameplay.

\n## Follow-up: PlayerAction decoder alignment (protocol 70)\n
The supplied runtime trace contains 28-byte PlayerAction bodies beginning
`ff ff ff ff 00 00 00 01 ...`. The protocol-70 local player entity ID is
`-1`, encoded here as a 32-bit value (`ff ff ff ff`), followed by the 32-bit
action. The previous Rust decoder read the entity ID as 64-bit, combining the
entity ID and action into one value. It then misread the clicked X coordinate
as the action and shifted all block coordinates. The decoder now reads the
32-bit entity ID and has a regression test for this exact layout.

This correction fixes PlayerAction parsing only. It does not prove that the
client's placement refusal is resolved: the supplied trace contains no
incoming UseItem (`0xaa`) or RemoveBlock (`0x9e`) packets. A live trace after
this correction is still required to determine why the client is refusing to
send placement/removal requests.

\n## V4: legacy packet alignment based on source and live trace\n
- PlayerAction is restored to the legacy fixed-width layout documented by the
  Prismarine/PocketMine packet class: 8-byte entity ID, then 32-bit action,
  x, y, z and face. The previous V3 32-bit entity-ID change was incorrect;
  it left four bytes unconsumed in the user's 28-byte packet.
- UseItem keeps the legacy xyz, face, two Vec3 values and Item-slot ordering.
  The decoder now accepts either the 44-byte legacy payload or the observed
  48-byte payload with a four-byte trailing field, preserving the decoded
  Item stack and logging the trailing value. It does not reinterpret that
  field as a selected hotbar slot.
- Face 255 is treated as an air-use/no-block-target action and is not passed
  into block-placement validation. A real block placement must arrive with a
  face in 0..=5.
- The user's supplied UseItem trace reports clicked coordinates (0,0,0) and
  face 255. That is not yet evidence of a block-targeted placement. The next
  live trace must show a UseItem packet with face 0..=5 to exercise placement.
- The uploaded environment has no Cargo executable, so compilation and Rust
  tests could not be run here. Do not deploy as verified until CI/cargo test
  passes and live client tests are completed.

- UseItem click/player vectors are decoded as little-endian IEEE-754 floats,
  matching the legacy PocketMine `UseItemPacket` decoder (its float reads use
  native little-endian on supported hosts). Integer coordinates remain
  big-endian.


## V5: deferred teleport retargeting

A source review found an edge case in the far-teleport chunk-stream path: if a
second teleport arrived while the first destination's chunks were still queued,
a second destination inside the newly selected chunk center could be sent to
the client immediately. The old pending stream could then finish and send its
latest server coordinates again, making the client appear to teleport twice
or intermittently remain at the wrong location.

The teleport path now preserves the deferred state while a chunk queue is still
active, clears stale teleport-confirmation targets while waiting, and sends the
latest destination only after the pending chunk stream drains. A regression test
was added for a far teleport followed by a near retarget during streaming.

This is a source-level change only. Cargo and rustc are not installed in this
environment, so compilation, unit tests, and live MCPE 0.14.3 verification
remain outstanding.


## V7: placement inventory consistency

Source review found that survival placement consumed an item even when
`set_block()` returned false, and it did not send a fresh inventory packet after
a successful consumption. Consumption also matched only the item ID, which can
select the wrong metadata variant for legacy blocks.

The placement path now consumes only after a successful world write, matches
item ID plus damage, and sends the updated inventory/equipment state. A client
reported item is trusted only if the exact ID/damage stack exists server-side;
otherwise the server-held stack is used. If the world write fails, the block and
inventory are resynchronized without charging the player. Added player-inventory
unit assertions for metadata-specific consumption.

This is a source-level correction, not proof of live protocol correctness. Run
`cargo test` and test survival placement with at least two metadata variants
(e.g. differently coloured wool) on a real MCPE 0.14.3 client.
