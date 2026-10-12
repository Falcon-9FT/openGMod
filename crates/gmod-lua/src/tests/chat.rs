//! The chat: the box and its keys, what is said going through the server to the clients, and
//! the box the client draws.

use super::*;
use crate::chat::{ENTRY, PANEL};

const KEY_Q: i32 = 27;
const KEY_U: i32 = 31;
const KEY_W: i32 = 33;
const KEY_Y: i32 = 35;
const KEY_ENTER: i32 = 64;
const KEY_BACKSPACE: i32 = 66;
const KEY_ESCAPE: i32 = 70;
const KEY_HOME: i32 = 74;
const KEY_PAGEUP: i32 = 76;
const KEY_LEFT: i32 = 89;
const MOUSE_WHEEL_UP: i32 = 112;

const BINDS: &str = "bind \"y\" \"messagemode\"\nbind \"u\" \"messagemode2\"\nbind \"q\" \"+menu\"\n";

/// The chat's hooks, listened to, and an `OnPlayerChat` like the base gamemode's.
const CHAT_LUA: &str = r#"
    HOOKS.StartChat = function(team) STARTED = team return HIDE end
    HOOKS.FinishChat = function() FINISHED = (FINISHED or 0) + 1 end
    HOOKS.ChatTextChanged = function(text) TYPED = text end
    HOOKS.PlayerBindPress = function(p, bind, down) BOUND = bind end
    team = { GetColor = function(t) return { r = 10, g = 20, b = 30, a = 255 } end }
    HOOKS.OnPlayerChat = function(p, text, team, dead)
        HEARD = { p, text, team, dead }
        chat.AddText(p, { r = 255, g = 255, b = 255, a = 255 }, ": " .. text)
        return true
    end
"#;

fn chat_client() -> Game {
    let g = client_game(&[("cfg/config_default.cfg", BINDS)]);
    g.runtime.run_string(CHAT_LUA, "chat").unwrap();
    g
}

fn press(g: &Game, code: i32) -> bool {
    let taken = g.runtime.button(code, true);
    g.runtime.button(code, false);
    taken
}

/// Opens the chat as the keyboard does: the key goes down with the letter it types.
fn open(g: &Game, code: i32, letter: &str) {
    assert!(g.runtime.button(code, true));
    assert!(g.runtime.type_text(letter));
    g.runtime.button(code, false);
}

/// A server with Alice in it, and her client with what the server has.
fn server_and_client() -> (Game, Game, EntityHandle) {
    let server = player_game();
    let alice = server.runtime.join(alice()).unwrap();
    server.runtime.think(0.02);
    let client = chat_client();
    client.runtime.apply_snapshot(&server.runtime.snapshot());
    client.runtime.set_local_player(alice);
    (server, client, alice)
}

/// What the server said to Alice reaches her client.
fn deliver(server: &Game, client: &Game, alice: EntityHandle) {
    for (to, message) in server.runtime.take_chat() {
        assert_eq!(to, alice);
        client.runtime.receive_chat(&message);
    }
}

fn said(speaker: EntityHandle, text: &str, team: bool, dead: bool) -> ChatMessage {
    ChatMessage::Said { speaker, text: text.into(), team, dead }
}

/// Vertices drawn in a colour, at any alpha.
fn drawn_rgb(g: &Game, rgb: [u8; 3]) -> Vec<[f32; 2]> {
    g.runtime.with_canvas(|canvas, _| canvas.vertices().iter().filter(|v| v.color[..3] == rgb).map(|v| v.pos).collect())
}

#[test]
fn the_chat_box_has_the_keys_while_it_is_open() {
    let g = chat_client();
    assert!(!g.runtime.chat_open() && !g.runtime.cursor_visible());
    // Y opens it, through `PlayerBindPress`; the Y is not typed
    assert!(g.runtime.button(KEY_Y, true));
    assert!(g.runtime.type_text("y"));
    g.runtime.button(KEY_Y, false);
    assert!(g.runtime.chat_open() && g.runtime.cursor_visible());
    assert!(g.eval::<bool>("return STARTED == false and BOUND == 'messagemode' and TYPED == nil"));

    // typing, with every change told
    assert!(g.runtime.type_text("hi there"));
    assert_eq!(g.eval::<String>("return TYPED"), "hi there");
    press(&g, KEY_BACKSPACE);
    assert_eq!(g.eval::<String>("return TYPED"), "hi ther");
    for _ in 0..4 {
        press(&g, KEY_LEFT);
    }
    g.runtime.type_text("\"");
    assert_eq!(g.eval::<String>("return TYPED"), "hi \"ther");
    // the game's keys are the box's: U does not open the team's chat now, W does not walk,
    // Q neither opens nor closes the spawn menu
    g.runtime
        .run_string("concommand.Add('+menu', function() MENU = 'open' end) concommand.Add('-menu', function() MENU = 'closed' end)", "menu")
        .unwrap();
    assert!(press(&g, KEY_U) && press(&g, KEY_W) && press(&g, KEY_Q));
    assert!(g.eval::<bool>("return STARTED == false and MENU == nil"));

    // Enter says it to the server and closes the box
    assert!(g.runtime.button(KEY_ENTER, true));
    assert!(g.runtime.type_text("\r"));
    g.runtime.button(KEY_ENTER, false);
    assert!(!g.runtime.chat_open() && !g.runtime.cursor_visible());
    assert_eq!(g.eval::<i64>("return FINISHED"), 1);
    assert_eq!(g.runtime.take_server_commands(), ["say \"hi 'ther\""]);

    // U: to the team
    open(&g, KEY_U, "u");
    g.runtime.type_text("go");
    press(&g, KEY_ENTER);
    assert!(g.eval::<bool>("return STARTED == true"));
    assert_eq!(g.runtime.take_server_commands(), ["say_team \"go\""]);

    // Esc closes it without a word; nothing typed, Enter says nothing
    open(&g, KEY_Y, "y");
    g.runtime.type_text("never");
    press(&g, KEY_ESCAPE);
    press(&g, KEY_Y);
    press(&g, KEY_ENTER);
    assert_eq!(g.eval::<i64>("return FINISHED"), 4);
    assert!(g.runtime.take_server_commands().is_empty());
    // closed, the keys are the game's again
    assert!(!press(&g, KEY_W) && !g.runtime.type_text("w"));

    // `chat.Open` and `chat.Close`, as Lua's own chat boxes call them
    g.runtime.run_string("chat.Open(2)", "open").unwrap();
    assert!(g.runtime.chat_open() && g.eval::<bool>("return STARTED == true"));
    g.runtime.run_string("chat.Close()", "close").unwrap();
    assert!(!g.runtime.chat_open() && g.eval::<i64>("return FINISHED") == 5);
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn what_is_said_goes_through_the_server_to_the_chat() {
    let (server, client, alice) = server_and_client();
    server.runtime.run_string("HOOKS.PlayerSay = function(p, text, team) SAID = { p, text, team } end", "say").unwrap();
    // typed into the box, sent as the client's command
    open(&client, KEY_Y, "y");
    client.runtime.type_text("hello world");
    press(&client, KEY_ENTER);
    for line in client.runtime.take_server_commands() {
        server.runtime.run_player_command(alice, &line);
    }
    assert!(server.eval::<bool>("return SAID[1] == Entity(1) and SAID[2] == 'hello world' and SAID[3] == false"));
    let chat = server.runtime.take_chat();
    assert_eq!(chat, [(alice, said(alice, "hello world", false, false))]);
    client.runtime.receive_chat(&chat[0].1);
    assert!(
        client.eval::<bool>("return HEARD[1] == LocalPlayer() and HEARD[2] == 'hello world' and HEARD[3] == false and HEARD[4] == false")
    );
    let lua = client.runtime.lua();
    assert_eq!(crate::chat::lines(lua), ["Alice: hello world"]);
    assert_eq!(crate::chat::last_pieces(lua)[0], ([10, 20, 30, 255], "Alice".to_string()));
    assert!(client.printed().contains("Alice: hello world\n"));

    // the terminal's `say`; `PlayerSay` changes the text, or keeps it quiet
    server
        .runtime
        .run_string("HOOKS.PlayerSay = function(p, text) if text == 'secret' then return '' end return text:upper() end", "say")
        .unwrap();
    server.runtime.run_player_command(alice, "say secret");
    assert!(server.runtime.take_chat().is_empty());
    server.runtime.run_player_command(alice, "say quiet please");
    assert_eq!(server.runtime.take_chat(), [(alice, said(alice, "QUIET PLEASE", false, false))]);
    // `Player:Say`, to the team: the speaker's own team hears it
    server.runtime.run_string("HOOKS.PlayerSay = nil Entity(1):Say('team talk', true)", "say").unwrap();
    assert_eq!(server.runtime.take_chat(), [(alice, said(alice, "team talk", true, false))]);
    // whom `PlayerCanSeePlayersChat` keeps it from does not get it
    server
        .runtime
        .run_string("HOOKS.PlayerCanSeePlayersChat = function(text, team, listener, speaker) return not team end", "see")
        .unwrap();
    server.runtime.run_player_command(alice, "say_team for the team");
    assert!(server.runtime.take_chat().is_empty());
    server.runtime.run_player_command(alice, "say \"for everyone\"");
    assert_eq!(server.runtime.take_chat(), [(alice, said(alice, "for everyone", false, false))]);
    // the dead say it dead
    server.runtime.run_player_command(alice, "kill");
    server.runtime.run_player_command(alice, "say boo");
    let chat = server.runtime.take_chat();
    assert_eq!(chat, [(alice, said(alice, "boo", false, true))]);

    // without `OnPlayerChat` taking it the engine shows the line itself
    client.runtime.run_string("HOOKS.OnPlayerChat = nil", "plain").unwrap();
    client.runtime.receive_chat(&chat[0].1);
    client.runtime.receive_chat(&said(alice, "to us", true, false));
    let lines = crate::chat::lines(lua);
    assert_eq!(lines[1..], ["*DEAD* Alice: boo", "(TEAM) Alice: to us"]);
    assert!(server.messages().is_empty(), "{:?}", server.messages());
    assert!(client.messages().is_empty(), "{:?}", client.messages());
}

#[test]
fn the_engine_tells_players_things_in_the_chat() {
    let (server, client, alice) = server_and_client();
    client.runtime.run_string("HOOKS.ChatText = function(i, name, text, kind) TOLD = kind return text == 'hidden' end", "told").unwrap();
    server
        .runtime
        .run_string(
            "Entity(1):ChatPrint('welcome') Entity(1):PrintMessage(HUD_PRINTTALK, 'hidden') Entity(1):PrintMessage(2, 'console')",
            "tell",
        )
        .unwrap();
    assert!(!server.printed().contains("welcome"));
    assert_eq!(server.runtime.take_to_clients(), [(alice, ToClient::Print("console".into()))]);
    deliver(&server, &client, alice);
    assert_eq!(client.eval::<String>("return TOLD"), "none");
    // `ChatText` returning true keeps a line out
    assert_eq!(crate::chat::lines(client.runtime.lua()), ["welcome"]);
    assert!(client.messages().is_empty(), "{:?}", client.messages());
}

#[test]
fn chat_add_text_makes_a_line_of_colours_and_players() {
    let (_server, client, _alice) = server_and_client();
    client
        .runtime
        .run_string(
            "chat.AddText({ r = 1, g = 2, b = 3, a = 255 }, 'a ', LocalPlayer(), ' b ', 5, ' ', true, {}, '!') chat.PlaySound()",
            "add",
        )
        .unwrap();
    let c = [1, 2, 3, 255];
    let pieces = crate::chat::last_pieces(client.runtime.lua());
    let expected: Vec<([u8; 4], String)> = [(c, "a "), ([10, 20, 30, 255], "Alice"), (c, " b "), (c, "5"), (c, " "), (c, "true"), (c, "!")]
        .map(|(c, s)| (c, s.to_string()))
        .into();
    assert_eq!(pieces, expected);
    assert!(client.printed().contains("a Alice b 5 true!\n"));
    assert!(client.runtime.take_sounds().iter().any(|s| matches!(s, crate::sound::SoundEvent::Play(p) if p.name.contains("talk"))));
    assert!(client.eval::<bool>(
        "local x, y = chat.GetChatBoxPos() local w, h = chat.GetChatBoxSize() return x == 20 and y == 140 and w == 480 and h == 240"
    ));
    assert!(client.messages().is_empty(), "{:?}", client.messages());
}

#[test]
fn the_chat_box_is_drawn_and_its_lines_fade() {
    let g = chat_client();
    let red = [200, 10, 10];
    let in_box = |p: &[f32; 2]| (20.0..=500.0).contains(&p[0]) && (140.0..=380.0).contains(&p[1]);
    g.runtime.run_string("chat.AddText({ r = 200, g = 10, b = 10, a = 255 }, 'a red line')", "add").unwrap();
    g.runtime.paint(0.016);
    let line = drawn_in(&g, [200, 10, 10, 255]);
    assert!(!line.is_empty() && line.iter().all(in_box), "{line:?}");
    assert!(drawn_in(&g, PANEL).is_empty());
    // a line too long for the box is cut into rows, at spaces or in a word too long
    g.runtime
        .run_string("chat.AddText({ r = 5, g = 200, b = 5, a = 255 }, string.rep('word ', 60), string.rep('x', 120))", "long")
        .unwrap();
    g.runtime.paint(0.0);
    let long = drawn_in(&g, [5, 200, 5, 255]);
    assert!(!long.is_empty() && long.iter().all(in_box), "{long:?}");
    let rows = long.iter().map(|p| p[1].round() as i32).collect::<std::collections::BTreeSet<_>>();
    assert!(rows.len() > 2, "{rows:?}");
    // closed, a line shows for `hud_saytext_time`, then fades away
    g.runtime.paint(11.0);
    assert!(!drawn_in(&g, [200, 10, 10, 255]).is_empty());
    g.runtime.paint(1.5);
    assert!(drawn_in(&g, [200, 10, 10, 255]).is_empty() && !drawn_rgb(&g, red).is_empty());
    g.runtime.paint(1.0);
    assert!(drawn_rgb(&g, red).is_empty());

    // open, the panel, the line typed, and the whole history
    open(&g, KEY_Y, "y");
    g.runtime.type_text("typing");
    g.runtime.paint(0.016);
    let panel = drawn_in(&g, PANEL);
    assert!(!panel.is_empty() && panel.iter().all(in_box), "{panel:?}");
    assert!(!drawn_in(&g, ENTRY).is_empty() && !drawn_in(&g, [200, 10, 10, 255]).is_empty());
    // more lines than fit: the oldest is scrolled back to, with PageUp and the wheel
    g.runtime.run_string("for i = 1, 40 do chat.AddText('line ', i) end", "more").unwrap();
    g.runtime.paint(0.016);
    assert!(drawn_in(&g, [200, 10, 10, 255]).is_empty());
    for _ in 0..10 {
        assert!(press(&g, KEY_PAGEUP));
    }
    assert!(press(&g, MOUSE_WHEEL_UP));
    g.runtime.paint(0.016);
    let line = drawn_in(&g, [200, 10, 10, 255]);
    assert!(!line.is_empty() && line.iter().all(in_box), "{line:?}");
    press(&g, KEY_ESCAPE);

    // `StartChat` returning true: Lua draws the box, the keys still go to it
    g.runtime.run_string("HIDE = true", "hide").unwrap();
    open(&g, KEY_Y, "y");
    g.runtime.type_text("hidden");
    g.runtime.paint(0.016);
    assert!(drawn_in(&g, PANEL).is_empty() && drawn_in(&g, ENTRY).is_empty());
    press(&g, KEY_ENTER);
    assert_eq!(g.runtime.take_server_commands(), ["say \"hidden\""]);

    // and `HUDShouldDraw("CHudChat")` hides it all
    g.runtime.run_string("HIDE = nil HOOKS.HUDShouldDraw = function(name) return name ~= 'CHudChat' end", "hide").unwrap();
    press(&g, KEY_Y);
    g.runtime.paint(0.016);
    assert!(drawn_in(&g, PANEL).is_empty());
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn a_client_tells_its_own_player_and_says_through_the_server() {
    let (server, client, alice) = server_and_client();
    // `ChatPrint` on a client goes into its box at once: nothing would take it from a queue
    client.runtime.run_string("LocalPlayer():ChatPrint('x') LocalPlayer():PrintMessage(HUD_PRINTTALK, 'y')", "tell").unwrap();
    assert_eq!(crate::chat::lines(client.runtime.lua()), ["x", "y"]);
    assert!(client.runtime.take_chat().is_empty());
    // what its player says goes to the server, as from the box
    client.runtime.run_string("LocalPlayer():Say('hello \"you\"', true)", "say").unwrap();
    assert!(client.runtime.take_chat().is_empty());
    assert_eq!(client.runtime.take_server_commands(), ["say_team \"hello 'you'\""]);
    assert!(client.eval::<bool>("return PrintMessage == nil"));

    // the server's `PrintMessage` is for every player (a game has the one)
    server.runtime.run_string("PrintMessage(HUD_PRINTTALK, 'to all') PrintMessage(2, 'to the console')", "print").unwrap();
    let told = ChatMessage::Text { player: 0, name: String::new(), text: "to all".into(), kind: "none".into() };
    assert_eq!(server.runtime.take_chat(), [(alice, told)]);
    assert_eq!(server.runtime.take_to_clients(), [(alice, ToClient::Print("to the console".into()))]);
    // and `MsgAll` to every console (the base gamemode's word of a player an NPC killed)
    server.runtime.run_string("MsgAll('Alice was killed by ', 7, '\\n')", "msg").unwrap();
    assert_eq!(server.runtime.take_to_clients(), [(alice, ToClient::Print("Alice was killed by 7\n".into()))]);
    assert!(server.messages().is_empty(), "{:?}", server.messages());
    assert!(client.messages().is_empty(), "{:?}", client.messages());
}

#[test]
fn a_name_in_the_chat_has_the_colour_the_gamemode_gives() {
    let (_server, client, alice) = server_and_client();
    // TTT gives the dead the colour of the living, so the chat does not tell who died
    client.runtime.run_string("HOOKS.GetTeamColor = function(p) return { r = 7, g = 8, b = 9, a = 255 } end", "color").unwrap();
    client.runtime.run_string("chat.AddText(LocalPlayer(), ' hi')", "add").unwrap();
    let alice_piece = ([7, 8, 9, 255], "Alice".to_string());
    assert_eq!(crate::chat::last_pieces(client.runtime.lua()).first(), Some(&alice_piece));
    // and the engine's own line, when `OnPlayerChat` does not take it
    client.runtime.run_string("HOOKS.OnPlayerChat = nil", "own").unwrap();
    client.runtime.receive_chat(&said(alice, "hey", false, true));
    assert!(crate::chat::last_pieces(client.runtime.lua()).contains(&alice_piece));
    // without the hook, the team's colour
    client.runtime.run_string("HOOKS.GetTeamColor = nil", "none").unwrap();
    client.runtime.receive_chat(&said(alice, "hey", false, false));
    assert_eq!(crate::chat::last_pieces(client.runtime.lua()).first(), Some(&([10, 20, 30, 255], "Alice".to_string())));
    assert!(client.messages().is_empty(), "{:?}", client.messages());
}

#[test]
fn a_long_line_typed_stays_in_the_box() {
    let g = chat_client();
    let white = [255, 255, 255, 255];
    open(&g, KEY_Y, "y");
    g.runtime.type_text(&"W".repeat(200));
    g.runtime.paint(0.016);
    // the strip ends 4 pixels in from the box's right edge, at 500
    let typed = drawn_in(&g, white);
    assert!(!typed.is_empty() && typed.iter().all(|p| (20.0..=496.0).contains(&p[0])), "{typed:?}");
    // the end of the line, and the caret, show
    assert!(typed.iter().any(|p| p[0] > 480.0), "{typed:?}");
    // the caret back at the start: the start shows
    press(&g, KEY_HOME);
    g.runtime.paint(0.016);
    let typed = drawn_in(&g, white);
    assert!(!typed.is_empty() && typed.iter().all(|p| (20.0..=496.0).contains(&p[0])), "{typed:?}");
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}
