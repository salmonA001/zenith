<!-- Generated from the command table in crates/zenith-agent/src/commands.rs by `zenith-ctl help --markdown`. -->
# zenith-ctl commands

Each command is also an MCP tool of the same name in `zenith-ctl mcp`.

## Observe

| Command | What it does | Arguments |
| --- | --- | --- |
| `state` | The player, the world and the crosshair target in one reply | none |
| `block` | The block at a position | `x`: block x<br>`y`: block y<br>`z`: block z |
| `blocks` | Counts of the non-air blocks in a box | `x1`: corner x<br>`y1`: corner y<br>`z1`: corner z<br>`x2`: opposite corner x<br>`y2`: opposite corner y<br>`z2`: opposite corner z |
| `find_block` | The nearest loaded blocks of a kind | `block`: block name, such as oak_log<br>`radius` (optional): search radius (default 32) |
| `entities` | Mobs, items and players near the player, nearest first | `radius` (optional): search radius (default 32)<br>`kind` (optional): only this kind |
| `inventory` | Everything the player carries, and the open screen | none |
| `recipes` | How an item is crafted | `item`: item name |
| `events` | What happened since a point, oldest first | `since` (optional): only events after this number |
| `messages` | Recent chat | `n` (optional): how many lines (default 20) |
| `screenshot` | Write the next rendered frame as a PNG (the game only, not a headless player) | `path`: where to write it |
| `frame` | Which screen is showing, its size, and how fast it is being drawn (the game only, not a headless player) | none |

## Session

| Command | What it does | Arguments |
| --- | --- | --- |
| `wait_ready` | Wait until the chunks around the player have arrived | `seconds` (optional): how long to wait (default 60) |
| `respawn` | Come back after dying | none |
| `quit` | Leave the world | none |

## Move

| Command | What it does | Arguments |
| --- | --- | --- |
| `goto` | Walk to a position, finding a path through loaded terrain | `x`: x<br>`y`: y<br>`z`: z<br>`near` (optional): arrive within this many blocks (default 1)<br>`timeout` (optional): give up after this many seconds (default 60)<br>`sprint` (optional): sprint |
| `walk` | Walk in a direction relative to the view for a time | `direction`: forward, back, left or right<br>`seconds`: how long<br>`sprint` (optional): sprint<br>`jump` (optional): keep jumping |
| `jump` | Jump once and reply when landed | none |
| `look` | Set the view direction | `yaw`: degrees, 0 faces south<br>`pitch`: degrees, 90 is straight down |
| `look_at` | Face a point | `x`: x<br>`y`: y<br>`z`: z |
| `fly` | Take off, or land again (creative mode only) | `off` (optional): land |

## Act

| Command | What it does | Arguments |
| --- | --- | --- |
| `break` | Mine a block, walking within reach first; replies with what it dropped | `x`: block x<br>`y`: block y<br>`z`: block z |
| `place` | Place a block at a position, against whatever is next to it | `x`: block x<br>`y`: block y<br>`z`: block z<br>`item`: which block, from the inventory |
| `interact` | Use what is in hand on a block: opens stations, flips levers | `x`: block x<br>`y`: block y<br>`z`: block z |
| `eat` | Eat the first food in the inventory | none |
| `use` | Use what is in hand where the view points: cast or pull a fishing rod, fill a bucket | none |
| `attack` | Hit an entity, walking within reach first | `entity`: entity id |
| `select` | Pick a hotbar slot | `slot`: 0 to 8 |
| `drop` | Throw what is in hand | `all` (optional): the whole stack |

## Inventory

| Command | What it does | Arguments |
| --- | --- | --- |
| `craft` | Craft an item from the inventory, using a bench you are next to if the recipe needs one | `item`: item name<br>`count` (optional): how many times (default 1) |
| `click_slot` | Click a slot of the open screen | `slot`: 0-35 inventory, 100+ the screen<br>`button` (optional): left, right or quick (default left) |
| `close_screen` | Close the open screen | none |

## Talk

| Command | What it does | Arguments |
| --- | --- | --- |
| `chat` | Say something, or run a /command | `text`: what to say |

## Screen

| Command | What it does | Arguments |
| --- | --- | --- |
| `key` | Press a key, as if at the keyboard (the game only, not a headless player) | `name`: a letter or digit, or space, enter, escape, backspace, shift, ctrl, slash, f1 to f12<br>`hold` (optional): keep it down until released<br>`release` (optional): let go of a held key |
| `type` | Type into the chat or the field that has focus (the game only, not a headless player) | `text`: what to type |
| `mouse` | Move the pointer, and click if asked, on the title screen and the screens over a world (the game only, not a headless player) | `x`: pixels from the left of a screenshot<br>`y`: pixels from the top of a screenshot<br>`left` (optional): click the left button<br>`right` (optional): click the right button<br>`shift` (optional): with shift held |
