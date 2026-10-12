//! A picture of a page: `page_shot <game folder> <url> <out.png> [seconds]`. The game folder
//! is served as `asset://garrysmod/`; what the page tells is printed.

use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() {
    if let Some(code) = gmod_web::subprocess() {
        std::process::exit(code);
    }
    let args: Vec<String> = std::env::args().collect();
    let [_, game, url, out, rest @ ..] = args.as_slice() else {
        eprintln!("page_shot <game folder> <url> <out.png> [seconds]");
        std::process::exit(2);
    };
    let seconds: f64 = rest.first().and_then(|s| s.parse().ok()).unwrap_or(5.0);
    let root = std::path::PathBuf::from(game);
    let files: gmod_web::Files =
        Arc::new(move |host, path| if host.eq_ignore_ascii_case("garrysmod") { std::fs::read(root.join(path)).ok() } else { None });
    let cache = std::env::temp_dir().join("opengmod-page-shot");
    let web = gmod_web::Web::start(&cache, &cache.join("chromium.log"), files).expect("chromium");
    let page = web.page(1280, 720).expect("a page");
    page.add_function("lua", "Run");
    page.add_function("lua", "PlaySound");
    page.add_function("language", "Update");
    page.open_url(url);
    let start = Instant::now();
    let mut picture = None;
    while start.elapsed().as_secs_f64() < seconds {
        for event in page.take_events() {
            println!("{event:?}");
            // the menu's phrases, answered as the game would
            if let gmod_web::PageEvent::Callback { function, callback: Some(id), args, .. } = &event
                && function == "Update"
            {
                let phrase = args.trim_matches(['[', ']', '"']).replace('_', " ");
                page.answer(*id, &format!("[\"{phrase}\"]"));
            }
        }
        if let Some(p) = page.take_picture() {
            picture = Some(p);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let picture = picture.expect("the page drew nothing");
    std::fs::write(out, gmod_material::png::encode(picture.width, picture.height, 4, &picture.rgba)).expect("write");
    drop(page);
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(10));
    }
}
