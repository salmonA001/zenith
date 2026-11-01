//! The one table of commands. The command-line help, argument checking, the MCP tools and the
//! reference page in `docs/` all come from it, so they cannot drift apart.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgKind {
    Int,
    Number,
    Text,
    Flag,
}

#[derive(Clone, Copy, Debug)]
pub struct ArgSpec {
    pub name: &'static str,
    pub kind: ArgKind,
    pub required: bool,
    pub help: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub struct CommandSpec {
    pub name: &'static str,
    pub group: &'static str,
    pub help: &'static str,
    pub args: &'static [ArgSpec],
    /// Whether this command changes the world, the player or the game.
    pub acts: bool,
    /// Needs a window with a GPU: only the windowed game answers it.
    pub needs_gpu: bool,
}

const fn req(name: &'static str, kind: ArgKind, help: &'static str) -> ArgSpec {
    ArgSpec { name, kind, required: true, help }
}
const fn opt(name: &'static str, kind: ArgKind, help: &'static str) -> ArgSpec {
    ArgSpec { name, kind, required: false, help }
}
const fn observe(name: &'static str, help: &'static str, args: &'static [ArgSpec]) -> CommandSpec {
    CommandSpec { name, group: "Observe", help, args, acts: false, needs_gpu: false }
}
const fn action(name: &'static str, group: &'static str, help: &'static str, args: &'static [ArgSpec]) -> CommandSpec {
    CommandSpec { name, group, help, args, acts: true, needs_gpu: false }
}

const XYZ: [ArgSpec; 3] =
    [req("x", ArgKind::Int, "block x"), req("y", ArgKind::Int, "block y"), req("z", ArgKind::Int, "block z")];

pub const COMMANDS: &[CommandSpec] = &[
    observe("state", "The player, the world and the crosshair target in one reply", &[]),
    observe("block", "The block at a position", &XYZ),
    observe(
        "blocks",
        "Counts of the non-air blocks in a box",
        &[
            req("x1", ArgKind::Int, "corner x"),
            req("y1", ArgKind::Int, "corner y"),
            req("z1", ArgKind::Int, "corner z"),
            req("x2", ArgKind::Int, "opposite corner x"),
            req("y2", ArgKind::Int, "opposite corner y"),
            req("z2", ArgKind::Int, "opposite corner z"),
        ],
    ),
    observe(
        "find_block",
        "The nearest loaded blocks of a kind",
        &[req("block", ArgKind::Text, "block name, such as oak_log"), opt("radius", ArgKind::Int, "search radius (default 32)")],
    ),
    observe(
        "entities",
        "Mobs, items and players near the player, nearest first",
        &[opt("radius", ArgKind::Number, "search radius (default 32)"), opt("kind", ArgKind::Text, "only this kind")],
    ),
    observe("inventory", "Everything the player carries, and the open screen", &[]),
    observe("recipes", "How an item is crafted", &[req("item", ArgKind::Text, "item name")]),
    observe("events", "What happened since a point, oldest first", &[opt("since", ArgKind::Int, "only events after this number")]),
    observe("messages", "Recent chat", &[opt("n", ArgKind::Int, "how many lines (default 20)")]),
    action(
        "wait_ready",
        "Session",
        "Wait until the chunks around the player have arrived",
        &[opt("seconds", ArgKind::Number, "how long to wait (default 60)")],
    ),
    action(
        "goto",
        "Move",
        "Walk to a position, finding a path through loaded terrain",
        &[
            req("x", ArgKind::Number, "x"),
            req("y", ArgKind::Number, "y"),
            req("z", ArgKind::Number, "z"),
            opt("near", ArgKind::Number, "arrive within this many blocks (default 1)"),
            opt("timeout", ArgKind::Number, "give up after this many seconds (default 60)"),
            opt("sprint", ArgKind::Flag, "sprint"),
        ],
    ),
    action(
        "walk",
        "Move",
        "Walk in a direction relative to the view for a time",
        &[
            req("direction", ArgKind::Text, "forward, back, left or right"),
            req("seconds", ArgKind::Number, "how long"),
            opt("sprint", ArgKind::Flag, "sprint"),
            opt("jump", ArgKind::Flag, "keep jumping"),
        ],
    ),
    action("jump", "Move", "Jump once and reply when landed", &[]),
    action(
        "look",
        "Move",
        "Set the view direction",
        &[req("yaw", ArgKind::Number, "degrees, 0 faces south"), req("pitch", ArgKind::Number, "degrees, 90 is straight down")],
    ),
    action("look_at", "Move", "Face a point", &[req("x", ArgKind::Number, "x"), req("y", ArgKind::Number, "y"), req("z", ArgKind::Number, "z")]),
    action("break", "Act", "Mine a block, walking within reach first; replies with what it dropped", &XYZ),
    action(
        "place",
        "Act",
        "Place a block at a position, against whatever is next to it",
        &[
            req("x", ArgKind::Int, "block x"),
            req("y", ArgKind::Int, "block y"),
            req("z", ArgKind::Int, "block z"),
            req("item", ArgKind::Text, "which block, from the inventory"),
        ],
    ),
    action("interact", "Act", "Use what is in hand on a block: opens stations, flips levers", &XYZ),
    action("eat", "Act", "Eat the first food in the inventory", &[]),
    action("attack", "Act", "Hit an entity, walking within reach first", &[req("entity", ArgKind::Int, "entity id")]),
    action("select", "Act", "Pick a hotbar slot", &[req("slot", ArgKind::Int, "0 to 8")]),
    action("drop", "Act", "Throw what is in hand", &[opt("all", ArgKind::Flag, "the whole stack")]),
    action(
        "craft",
        "Inventory",
        "Craft an item from the inventory, using a bench you are next to if the recipe needs one",
        &[req("item", ArgKind::Text, "item name"), opt("count", ArgKind::Int, "how many times (default 1)")],
    ),
    action(
        "click_slot",
        "Inventory",
        "Click a slot of the open screen",
        &[req("slot", ArgKind::Int, "0-35 inventory, 100+ the screen"), opt("button", ArgKind::Text, "left, right or quick (default left)")],
    ),
    action("close_screen", "Inventory", "Close the open screen", &[]),
    action("chat", "Talk", "Say something, or run a /command", &[req("text", ArgKind::Text, "what to say")]),
    action("respawn", "Session", "Come back after dying", &[]),
    action("quit", "Session", "Leave the world", &[]),
    CommandSpec {
        name: "screenshot",
        group: "Observe",
        help: "Write the next rendered frame as a PNG",
        args: &[req("path", ArgKind::Text, "where to write it")],
        acts: false,
        needs_gpu: true,
    },
];

pub fn find(name: &str) -> Option<&'static CommandSpec> {
    let name = name.replace('-', "_");
    COMMANDS.iter().find(|c| c.name == name)
}

/// The reference page, as `docs/commands.md` holds it.
pub fn reference_markdown() -> String {
    let mut out = String::from(
        "<!-- Generated from the command table in crates/zenith-agent/src/commands.rs by `zenith-ctl help --markdown`. -->\n\
         # zenith-ctl commands\n\n\
         Each command is also an MCP tool of the same name in `zenith-ctl mcp`.\n",
    );
    let mut groups: Vec<&str> = Vec::new();
    for c in COMMANDS {
        if !groups.contains(&c.group) {
            groups.push(c.group);
        }
    }
    for group in groups {
        out.push_str(&format!("\n## {group}\n\n| Command | What it does | Arguments |\n| --- | --- | --- |\n"));
        for c in COMMANDS.iter().filter(|c| c.group == group) {
            let args: Vec<String> = c
                .args
                .iter()
                .map(|a| format!("`{}`{}: {}", a.name, if a.required { "" } else { " (optional)" }, a.help))
                .collect();
            let gpu = if c.needs_gpu { " (windowed game only)" } else { "" };
            out.push_str(&format!(
                "| `{}` | {}{} | {} |\n",
                c.name,
                c.help,
                gpu,
                if args.is_empty() { "none".to_string() } else { args.join("<br>") }
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_names_are_unique_and_lookups_accept_dashes() {
        for (i, a) in COMMANDS.iter().enumerate() {
            assert!(COMMANDS[i + 1..].iter().all(|b| b.name != a.name), "{} twice", a.name);
        }
        assert_eq!(find("find-block").map(|c| c.name), Some("find_block"));
        assert!(find("nope").is_none());
    }

    #[test]
    fn required_arguments_come_before_optional_ones() {
        // The command line takes them by position.
        for c in COMMANDS {
            let mut optional = false;
            for a in c.args {
                assert!(!(optional && a.required), "{}: {} is required after an optional one", c.name, a.name);
                optional |= !a.required;
            }
        }
    }

    #[test]
    fn the_reference_mentions_every_command() {
        let page = reference_markdown();
        for c in COMMANDS {
            assert!(page.contains(&format!("`{}`", c.name)));
        }
    }
}
