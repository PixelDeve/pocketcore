#![allow(dead_code)]

mod bedwars;
mod binary;
mod blocks;
mod commands;
mod config;
mod creative;
mod entity;
mod game;
mod protocol;
mod raknet;
mod scripting;
mod state;
mod world;

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use game::GameServer;
use protocol::Quirks;
use raknet::server::{RakConfig, RakServer};
use scripting::ScriptConfig;
use state::{GameConfig, Pvp};
use world::{Generator, World};

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

/// The original template map: `maps/<map>.pcw`, else import the MCPE world folder `maps/<map>/`
/// (LevelDB) and cache it as `.pcw`, else an empty generated world to build the map in.
fn load_template(
    cfg: &config::ServerConfig,
    bw: &bedwars::BwConfig,
    generator: Generator,
    limit: Option<i32>,
    spawn: (i32, i32, i32),
    order_byte: bool,
) -> io::Result<World> {
    use std::path::PathBuf;
    use world::{leveldb, pcw};
    let maps: PathBuf = bw.dir.join("maps");
    std::fs::create_dir_all(&maps)?;
    let name = &cfg.bedwars.map;
    let pcw_path = maps.join(format!("{name}.pcw"));
    let folder = maps.join(name);
    const PAD: i32 = 2;
    if pcw_path.is_file() {
        let w = pcw::load(&pcw_path, order_byte, PAD)
            .map_err(|e| invalid(format!("{}: {e}", pcw_path.display())))?;
        log::info!("template map loaded from {}", pcw_path.display());
        return Ok(w);
    }
    if folder.join("db").is_dir() {
        let w = leveldb::import(&folder, order_byte, PAD)
            .map_err(|e| invalid(format!("importing {}: {e}", folder.display())))?;
        pcw::save(&w, &pcw_path)?;
        log::info!("imported {} and saved {}", folder.display(), pcw_path.display());
        return Ok(w);
    }
    log::warn!(
        "no template map found: put a world at {} (MCPE world folder) or {}. Using a generated world.",
        folder.display(),
        pcw_path.display()
    );
    Ok(World::new(generator, limit, spawn, order_byte))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> io::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();
    // lets you check in the console that the new binary (not an old one) is running
    log::info!("pocketcore build marker: pocketcore-v10 (slime platform fix, safe TNT)");

    let cfg = config::load("server.toml")?;
    let mut bind: SocketAddr = cfg.network.bind.parse().map_err(|e| invalid(format!("network.bind: {e}")))?;
    // Pterodactyl exports the allocated port as SERVER_PORT; it overrides server.toml.
    if let Some(port) = std::env::var("SERVER_PORT").ok().and_then(|p| p.parse::<u16>().ok()) {
        bind.set_port(port);
    }
    let generator = match cfg.world.generator.as_str() {
        "flat" => Generator::Flat,
        "void" => Generator::Void,
        other => return Err(invalid(format!("world.generator: unknown generator {other:?}"))),
    };
    let tail = config::parse_hex(&cfg.protocol.start_game_tail)
        .map_err(|e| invalid(format!("protocol.start_game_tail: {e}")))?;
    let quirks = Quirks {
        chunk_order_byte: cfg.protocol.chunk_order_byte,
        start_game_tail: tail,
        join_extras: cfg.protocol.join_extras,
        legacy_adventure_flags: !cfg.protocol.adventure_flags.eq_ignore_ascii_case("modern"),
        spawn_respawn: cfg.protocol.spawn_respawn,
        adventure_permission: cfg.protocol.adventure_permission,
        inventory_placeholders: cfg.protocol.inventory_placeholders,
        enforce_break_time: cfg.protocol.enforce_break_time,
    };

    let limit = (cfg.world.limit_chunks > 0).then_some(cfg.world.limit_chunks);
    let [sx, sy, sz] = cfg.world.spawn;
    let bw_cfg = cfg.bedwars.to_bw();
    let world = load_template(&cfg, &bw_cfg, generator, limit, (sx, sy, sz), quirks.chunk_order_byte)?;

    let plugin_dir = std::path::PathBuf::from(&cfg.scripting.dir);
    std::fs::create_dir_all(&plugin_dir)?;

    let mut game = GameServer::new(
        GameConfig {
            max_players: cfg.server.max_players as usize,
            max_view: cfg.server.view_distance.max(1),
            gamemode: cfg.server.default_gamemode,
            quirks,
            trace: cfg.protocol.trace_packets,
            hotbar_offset: cfg.protocol.hotbar_offset,
            pvp: Pvp {
                knockback_h: cfg.pvp.knockback_horizontal,
                knockback_v: cfg.pvp.knockback_vertical,
                attack_cooldown_ticks: cfg.pvp.attack_cooldown_ticks,
                max_reach: cfg.pvp.max_reach,
            },
            broadcast_radius: cfg.server.broadcast_radius,
        },
        world,
        ScriptConfig {
            enabled: cfg.scripting.enabled,
            dir: plugin_dir,
            reload_check_ticks: cfg.scripting.reload_check_ticks,
        },
        bw_cfg,
    );
    // Console input (Pterodactyl console / terminal): `op Name`, `ban Name`, ...
    let (console_tx, console_rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(l) => {
                    if console_tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    game.set_console(console_rx);
    game.start();

    let rak = RakConfig {
        bind,
        name: cfg.server.name,
        max_players: cfg.server.max_players,
        max_mtu: cfg.network.max_mtu,
        session_timeout: Duration::from_secs(cfg.network.session_timeout_secs),
        max_per_ip: cfg.network.max_connections_per_ip,
    };

    let server = RakServer::bind(rak, game).await?;
    tokio::select! {
        res = server.run() => res,
        _ = tokio::signal::ctrl_c() => {
            log::info!("shutting down");
            Ok(())
        }
    }
}
