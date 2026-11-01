//! Chat commands, from a player or the server console.
use crate::Server;
use crate::rules;
use zenith_content::{BlockState, ItemStack};
use zenith_core::glam::DVec3;
use zenith_core::BlockPos;
use zenith_protocol::{EntityKind, GameMode};

const HELP: &str = "/help /list /seed /tps /time set <day|night|noon|midnight|ticks> /weather <clear|rain> \
/gamemode <survival|creative> /give <item> [count] /tp <x y z | player> /spawn /setspawn /kill \
/setblock <x y z block> /fill <x1 y1 z1 x2 y2 z2 block> /summon <mob> [count] /save /say <text> /op <name> \
/deop <name> /kick <name>";

/// Most blocks one `/fill` may touch.
const MAX_FILL: i64 = 256 * 256 * 16;

/// Who a command is running for: a player by index, or the console.
pub type Sender = Option<usize>;

/// Runs a command line (without its slash) and returns what to tell the sender.
pub fn run(server: &mut Server, sender: Sender, line: &str) -> Vec<String> {
    let mut args = line.split_whitespace();
    let Some(name) = args.next() else { return vec![] };
    let args: Vec<&str> = args.collect();
    let is_op = sender.is_none_or(|i| server.players.get(i).is_some_and(|p| p.op));
    let free = matches!(name, "help" | "list" | "seed" | "tps" | "spawn" | "kill");
    if !free && !is_op {
        return vec!["You need to be an operator for that.".into()];
    }
    let result = match name {
        "help" => Ok(vec![HELP.to_string()]),
        "list" => {
            let names: Vec<&str> = server.players.iter().map(|p| p.name.as_str()).collect();
            Ok(vec![format!("{} online: {}", names.len(), names.join(", "))])
        }
        "seed" => Ok(vec![format!("Seed: {}", server.world.generator.seed)]),
        "tps" => {
            let s = server.stats.summary();
            Ok(vec![format!(
                "{:.1} ticks/s, tick {:.2} ms average, {:.2} ms p99, {} chunks, {} entities",
                s.tps,
                s.mean_ms,
                s.p99_ms,
                server.world.map.chunks.len(),
                server.entities.list.len()
            )])
        }
        "time" => time(server, &args),
        "weather" => match args.first().copied() {
            Some("clear") => {
                server.clock.rain = 0;
                server.rain_target = 0;
                server.next_weather = server.world.tick + 12_000;
                Ok(vec!["The sky clears.".into()])
            }
            Some("rain") => {
                server.rain_target = 255;
                server.next_weather = server.world.tick + 6_000;
                Ok(vec!["It starts to rain.".into()])
            }
            _ => Err("/weather <clear|rain>".to_string()),
        },
        "gamemode" => gamemode(server, sender, &args),
        "give" => give(server, sender, &args),
        "tp" => tp(server, sender, &args),
        "spawn" => match sender {
            Some(i) => {
                let at = server.spawn_point();
                server.teleport(i, at);
                Ok(vec!["Back to spawn.".into()])
            }
            None => Err("Only a player can go somewhere.".into()),
        },
        "setspawn" => match sender {
            Some(i) => {
                server.spawn = BlockPos::from_vec(server.players[i].body.pos);
                Ok(vec![format!("Spawn set to {:?}.", server.spawn)])
            }
            None => Err("Only a player stands somewhere.".into()),
        },
        "kill" => match sender {
            Some(i) => {
                let p = &mut server.players[i];
                let mode = p.mode;
                p.mode = GameMode::Survival;
                p.take_damage_absolute(&mut server.world);
                p.mode = mode;
                Ok(vec![])
            }
            None => Err("The console has nothing to lose.".into()),
        },
        "setblock" => setblock(server, &args),
        "fill" => fill(server, &args),
        "summon" => summon(server, sender, &args),
        "save" => server.save().map(|n| vec![format!("Saved {n} chunks.")]).map_err(|e| e.to_string()),
        "say" => {
            let text = args.join(" ");
            let from = sender.map(|i| server.players[i].name.clone());
            server.broadcast_chat(from, text);
            Ok(vec![])
        }
        "op" | "deop" => match args.first() {
            Some(target) => match server.players.iter_mut().find(|p| p.name == *target) {
                Some(p) => {
                    p.op = name == "op";
                    Ok(vec![format!("{target} is {} an operator.", if p.op { "now" } else { "no longer" })])
                }
                None => Err(format!("{target} is not here.")),
            },
            None => Err(format!("/{name} <name>")),
        },
        "kick" => match args.first() {
            Some(target) => match server.players.iter().position(|p| p.name == *target) {
                Some(i) => {
                    server.players[i].peer.send(&zenith_protocol::S2C::Bye("Kicked by an operator.".into()));
                    server.players[i].peer.close();
                    Ok(vec![format!("Kicked {target}.")])
                }
                None => Err(format!("{target} is not here.")),
            },
            None => Err("/kick <name>".to_string()),
        },
        _ => Err(format!("Unknown command /{name}. Try /help.")),
    };
    result.unwrap_or_else(|e| vec![e])
}

fn time(server: &mut Server, args: &[&str]) -> Result<Vec<String>, String> {
    let value = match args {
        ["set", "day"] => 1000,
        ["set", "noon"] => 6000,
        ["set", "night"] => 13000,
        ["set", "midnight"] => 18000,
        ["set", number] => number.parse::<u32>().map_err(|_| format!("{number} is not a time."))? % 24000,
        _ => return Err("/time set <day|night|noon|midnight|ticks>".into()),
    };
    server.clock.time = value;
    Ok(vec![format!("Time set to {value}.")])
}

fn gamemode(server: &mut Server, sender: Sender, args: &[&str]) -> Result<Vec<String>, String> {
    let i = sender.ok_or("The console has no game mode.")?;
    let mode = match args.first().copied() {
        Some("survival" | "s") => GameMode::Survival,
        Some("creative" | "c") => GameMode::Creative,
        _ => return Err("/gamemode <survival|creative>".into()),
    };
    server.players[i].mode = mode;
    Ok(vec![format!("Game mode set to {mode:?}.")])
}

fn give(server: &mut Server, sender: Sender, args: &[&str]) -> Result<Vec<String>, String> {
    let i = sender.ok_or("The console has no pockets.")?;
    let name = *args.first().ok_or("/give <item> [count]")?;
    let item = server.world.reg.item_by_name(name).ok_or_else(|| format!("There is no item called {name}."))?;
    let count = args.get(1).map_or(Ok(1), |c| c.parse::<u32>()).map_err(|_| "That is not a number.".to_string())?;
    let reg = server.world.reg.clone();
    let cap = reg.item(item).map_or(1, |d| d.max_stack.max(1)) as u32;
    let mut left = count.min(36 * cap);
    while left > 0 {
        let n = left.min(cap) as u8;
        if server.players[i].inv.add(&reg, ItemStack::new(item, n)).is_some() {
            break;
        }
        left -= n as u32;
    }
    Ok(vec![format!("Gave {count} {name}.")])
}

fn tp(server: &mut Server, sender: Sender, args: &[&str]) -> Result<Vec<String>, String> {
    let i = sender.ok_or("Only a player can go somewhere.")?;
    let at = match args {
        [x, y, z] => {
            let n = |s: &str| s.parse::<f64>().map_err(|_| format!("{s} is not a number."));
            DVec3::new(n(x)?, n(y)?, n(z)?)
        }
        [name] => server.players.iter().find(|p| p.name == *name).map(|p| p.body.pos).ok_or_else(|| format!("{name} is not here."))?,
        _ => return Err("/tp <x y z | player>".into()),
    };
    server.teleport(i, at);
    Ok(vec![format!("Teleported to {:.1} {:.1} {:.1}.", at.x, at.y, at.z)])
}

fn block_state(server: &Server, name: &str) -> Result<BlockState, String> {
    server.world.reg.block_by_name(name).map(BlockState::of).ok_or_else(|| format!("There is no block called {name}."))
}

fn int(s: &str) -> Result<i32, String> {
    s.parse::<i32>().map_err(|_| format!("{s} is not a whole number."))
}

fn setblock(server: &mut Server, args: &[&str]) -> Result<Vec<String>, String> {
    let [x, y, z, name] = args else { return Err("/setblock <x y z block>".into()) };
    let state = block_state(server, name)?;
    let p = BlockPos::new(int(x)?, int(y)?, int(z)?);
    if !server.world.set(p, state) {
        return Err("Nothing changed: is that chunk loaded?".into());
    }
    rules::drain_updates(&mut server.world);
    Ok(vec![format!("Set {name} at {} {} {}.", p.x, p.y, p.z)])
}

fn fill(server: &mut Server, args: &[&str]) -> Result<Vec<String>, String> {
    let [x1, y1, z1, x2, y2, z2, name] = args else { return Err("/fill <x1 y1 z1 x2 y2 z2 block>".into()) };
    let state = block_state(server, name)?;
    let (x1, y1, z1, x2, y2, z2) = (int(x1)?, int(y1)?, int(z1)?, int(x2)?, int(y2)?, int(z2)?);
    let (lx, hx, ly, hy, lz, hz) = (x1.min(x2), x1.max(x2), y1.min(y2), y1.max(y2), z1.min(z2), z1.max(z2));
    let total = (hx - lx + 1) as i64 * (hy - ly + 1) as i64 * (hz - lz + 1) as i64;
    if total > MAX_FILL {
        return Err(format!("That is {total} blocks; the limit is {MAX_FILL}."));
    }
    let mut changed = 0;
    for y in ly..=hy {
        for z in lz..=hz {
            for x in lx..=hx {
                if server.world.set(BlockPos::new(x, y, z), state) {
                    changed += 1;
                }
            }
        }
    }
    Ok(vec![format!("Filled {changed} blocks.")])
}

fn summon(server: &mut Server, sender: Sender, args: &[&str]) -> Result<Vec<String>, String> {
    let i = sender.ok_or("Only a player stands somewhere.")?;
    let name = *args.first().ok_or("/summon <mob> [count]")?;
    let kind = EntityKind::from_name(name).ok_or_else(|| format!("There is no mob called {name}."))?;
    let count = args.get(1).map_or(Ok(1), |c| c.parse::<u32>()).map_err(|_| "That is not a number.".to_string())?.min(64);
    let at = server.players[i].body.pos + server.players[i].last_input.look() * 3.0 + DVec3::Y;
    for n in 0..count {
        let spread = DVec3::new((n % 8) as f64 - 4.0, 0.0, (n / 8) as f64) * 0.5;
        if kind == EntityKind::Tnt {
            server.entities.spawn(kind, at + spread, crate::entity::Data::Tnt { fuse: 60 });
        } else {
            server.entities.spawn_mob(kind, at + spread);
        }
    }
    Ok(vec![format!("Summoned {count} {name}.")])
}
