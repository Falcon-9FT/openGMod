//! Signs in to Steam as Garry's Mod and says who the player is, and what the workshop says of
//! the items given (their IDs) and of their authors:
//! `cargo run -p gmod-steam --example whoami -- <GarrysMod folder> [ID ...]`.

use std::time::{Duration, Instant};

use gmod_steam::{Event, Steam};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(root) = args.next() else {
        eprintln!("whoami <GarrysMod folder> [workshop ID ...]");
        std::process::exit(2);
    };
    let ids: Vec<u64> = args.filter_map(|a| a.parse().ok()).collect();
    let steam = match Steam::start(std::path::Path::new(&root)) {
        Ok(steam) => steam,
        Err(e) => {
            eprintln!("No Steam: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "{} ({}), owns Garry's Mod; {} workshop items subscribed ({} with those switched off)",
        steam.name(),
        steam.steam_id(),
        steam.subscribed(false).len(),
        steam.subscribed(true).len()
    );
    let request = steam.request_details(&ids);
    let mut names = 0;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        for event in steam.run_frame() {
            match event {
                Event::Details { request: r, items } if Some(r) == request => match items {
                    Ok(items) => {
                        for item in items {
                            println!(
                                "{}: {:?} by {}, {} bytes, +{} -{}, tags {:?}, preview {}",
                                item.id, item.title, item.owner, item.size, item.votes_up, item.votes_down, item.tags, item.preview_url
                            );
                            names += 1;
                            if let Some(name) = steam.player_name(item.owner) {
                                println!("  {} is {name:?}", item.owner);
                                names -= 1;
                            }
                        }
                    }
                    Err(e) => println!("The details failed: EResult {e}"),
                },
                Event::Name(id) => {
                    println!("  {id} is {:?}", steam.known_name(id));
                    names -= 1;
                }
                other => println!("{other:?}"),
            }
        }
        if request.is_none() || (names == 0 && started.elapsed() > Duration::from_secs(1)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
