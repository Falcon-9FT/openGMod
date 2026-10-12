//! The map's entities talking to each other: outputs, the logic entities, `AddOutput`,
//! `ent_fire`.

use super::*;

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// A server with [`ENTITY_LUA`] that prints every input given to an entity named `rec*`:
/// the entity, the input, its value and who sent it.
fn recording() -> Game {
    let g = game(Realm::Server, &[("lua/includes/init.lua", ENTITY_LUA)]);
    g.runtime.init();
    g.runtime
        .run_string(
            "HOOKS.AcceptInput = function(ent, input, activator, caller, param)
                 if string.sub(ent:GetName(), 1, 3) == 'rec' then
                     print(ent:GetName(), input, param, caller:IsValid() and caller:GetName() or '-')
                 end
             end",
            "rec",
        )
        .unwrap();
    g
}

fn map(entities: Vec<Vec<(String, String)>>) -> Map {
    let mut all = vec![pairs(&[("classname", "worldspawn")])];
    all.extend(entities);
    Map { name: "gm_io".into(), entities: all, brush_models: vec![([-512.0; 3], [512.0; 3])], ..Map::default() }
}

/// Lines printed since the last call.
fn taken(g: &Game, seen: &mut usize) -> Vec<String> {
    let printed = g.printed();
    let new = printed[*seen..].lines().map(str::to_string).collect();
    *seen = printed.len();
    new
}

#[test]
fn outputs_wire_the_map_together() {
    let g = recording();
    g.runtime.spawn_map(&map(vec![
        pairs(&[("classname", "logic_auto"), ("OnMapSpawn", "relay1,Trigger,,0,-1"), ("spawnflags", "1")]),
        // two connections of one output, one of them in the newer form, used once
        pairs(&[
            ("classname", "logic_relay"),
            ("targetname", "relay1"),
            ("OnTrigger", "counter,Add,5,0.5,-1"),
            ("OnTrigger", "rec_relay\u{1b}Seen\u{1b}first\u{1b}0\u{1b}1"),
        ]),
        pairs(&[
            ("classname", "math_counter"),
            ("targetname", "counter"),
            ("max", "10"),
            ("OutValue", "rec_counter,Value,,0,-1"),
            ("OnHitMax", "timer,Enable,,0,-1"),
        ]),
        pairs(&[
            ("classname", "logic_timer"),
            ("targetname", "timer"),
            ("StartDisabled", "1"),
            ("RefireTime", "1"),
            ("OnTimer", "rec_timer,Tick,,0,2"),
        ]),
        pairs(&[("classname", "info_target"), ("targetname", "rec_relay")]),
        pairs(&[("classname", "info_target"), ("targetname", "rec_counter")]),
        pairs(&[("classname", "info_target"), ("targetname", "rec_timer")]),
    ]));
    let mut seen = 0;
    assert!(g.eval::<bool>("return #ents.FindByClass('logic_auto') == 0"), "removed after it fired");
    g.runtime.think(0.1);
    assert_eq!(taken(&g, &mut seen), ["rec_relay\tSeen\tfirst\trelay1"], "the map spawned, the relay passed it on");
    g.runtime.think(0.5);
    assert_eq!(taken(&g, &mut seen), ["rec_counter\tValue\t5\tcounter"], "half a second later the counter has 5 and says so");
    // the relay again: once only for the second connection, so only the counter hears it
    g.runtime.run_string("ents.FindByName('relay1')[1]:Fire('Trigger')", "again").unwrap();
    g.runtime.think(0.7);
    assert_eq!(taken(&g, &mut seen), ["rec_counter\tValue\t10\tcounter"]);
    // at its top the counter turns the timer on: a tick a second, twice, then no more
    for ticks in [1, 1, 0] {
        g.runtime.think(1.0);
        assert_eq!(taken(&g, &mut seen).len(), ticks);
    }
    // past its top the counter stays there
    g.runtime.run_string("ents.FindByName('counter')[1]:Fire('Add', '7')", "more").unwrap();
    g.runtime.think(0.1);
    assert_eq!(taken(&g, &mut seen), ["rec_counter\tValue\t10\tcounter"]);
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn relays_branches_and_cases() {
    let g = recording();
    g.runtime.spawn_map(&map(vec![
        pairs(&[("classname", "logic_relay"), ("targetname", "slow"), ("OnTrigger", "rec,Slow,,1,-1")]),
        pairs(&[("classname", "logic_relay"), ("targetname", "fast"), ("spawnflags", "2"), ("OnTrigger", "rec,Fast,,1,-1")]),
        pairs(&[("classname", "logic_relay"), ("targetname", "off"), ("StartDisabled", "1"), ("OnTrigger", "rec,Off,,0,-1")]),
        pairs(&[("classname", "logic_branch"), ("targetname", "branch"), ("OnTrue", "rec,True,,0,-1"), ("OnFalse", "rec,False,,0,-1")]),
        pairs(&[
            ("classname", "logic_compare"),
            ("targetname", "cmp"),
            ("CompareValue", "3"),
            ("OnLessThan", "rec,Less,,0,-1"),
            ("OnEqualTo", "rec,Equal,,0,-1"),
            ("OnNotEqualTo", "rec,NotEqual,,0,-1"),
            ("OnGreaterThan", "rec,Greater,,0,-1"),
        ]),
        pairs(&[
            ("classname", "logic_case"),
            ("targetname", "case"),
            ("Case01", "red"),
            ("Case03", "blue"),
            ("OnCase01", "rec,Red,,0,-1"),
            ("OnCase03", "rec,Blue,,0,-1"),
            ("OnDefault", "rec,Other,,0,-1"),
        ]),
        pairs(&[("classname", "info_target"), ("targetname", "rec")]),
    ]));
    let mut seen = 0;
    let fire = |code: &str| g.runtime.run_string(code, "fire").unwrap();
    fire(
        "local function f(name, input, param) ents.FindByName(name)[1]:Fire(input, param) end
         f('slow', 'Trigger') f('fast', 'Trigger') f('off', 'Trigger')",
    );
    g.runtime.think(0.1);
    // while its input waits, the slow relay does not fire again; the fast one does
    fire("ents.FindByName('slow')[1]:Fire('Trigger') ents.FindByName('fast')[1]:Fire('Trigger')");
    g.runtime.think(1.5);
    assert_eq!(taken(&g, &mut seen), ["rec\tSlow\t\tslow", "rec\tFast\t\tfast", "rec\tFast\t\tfast"]);
    // CancelPending drops what waits; an output added with AddOutput, twice only
    fire(
        "local slow = ents.FindByName('slow')[1]
         slow:Fire('Trigger') slow:Fire('CancelPending', '', 0.5)
         local off = ents.FindByName('off')[1]
         off:Fire('AddOutput', 'OnTrigger rec:Added:x:0:2') off:Fire('Enable')
         for i = 1, 3 do off:Fire('Trigger', '', 0.1 * i) end",
    );
    g.runtime.think(2.0);
    assert_eq!(taken(&g, &mut seen), ["rec\tOff\t\toff", "rec\tAdded\tx\toff", "rec\tOff\t\toff", "rec\tAdded\tx\toff", "rec\tOff\t\toff"]);
    fire(
        "local function f(name, input, param) ents.FindByName(name)[1]:Fire(input, param) end
         f('branch', 'Test') f('branch', 'SetValueTest', '1') f('branch', 'ToggleTest')
         f('cmp', 'SetValueCompare', '2') f('cmp', 'SetValueCompare', '3') f('cmp', 'SetValue', '9') f('cmp', 'Compare')
         f('case', 'InValue', 'BLUE') f('case', 'InValue', 'green')",
    );
    g.runtime.think(0.1);
    let lines: Vec<String> = taken(&g, &mut seen).iter().map(|l| l.split('\t').skip(1).take(2).collect::<Vec<_>>().join(" ")).collect();
    assert_eq!(lines, ["False ", "True ", "False ", "Less 2", "NotEqual 2", "Equal 3", "Greater 9", "NotEqual 9", "Blue ", "Other green"]);
    // a shuffled pick gives each case once before any comes again
    fire("for i = 1, 4 do ents.FindByName('case')[1]:Fire('PickRandomShuffle') end");
    g.runtime.think(0.1);
    let mut picks: Vec<String> = taken(&g, &mut seen).iter().map(|l| l.split('\t').nth(1).unwrap_or("").to_string()).collect();
    picks[..2].sort();
    picks[2..].sort();
    assert_eq!(picks, ["Blue", "Red", "Blue", "Red"]);
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn targets_by_class_and_by_the_engines_names() {
    let g = recording();
    g.runtime.spawn_map(&map(vec![
        pairs(&[
            ("classname", "logic_relay"),
            ("targetname", "relay"),
            ("OnTrigger", "!activator,Activated,,0,-1"),
            ("OnTrigger", "!self,FireUser1,,0,-1"),
            ("OnUser1", "rec*,User,,0,-1"),
            ("OnUser1", "info_target,Class,,0,-1"),
        ]),
        pairs(&[("classname", "info_target"), ("targetname", "rec_a")]),
        pairs(&[("classname", "info_target"), ("targetname", "rec_b")]),
        pairs(&[("classname", "info_target"), ("targetname", "rec_activator")]),
    ]));
    let mut seen = 0;
    g.runtime.run_string("ents.FindByName('relay')[1]:Fire('Trigger', '', 0, ents.FindByName('rec_activator')[1])", "fire").unwrap();
    g.runtime.think(0.2);
    let lines: Vec<String> = taken(&g, &mut seen).iter().map(|l| l.replace('\t', " ")).collect();
    assert_eq!(
        lines,
        [
            "rec_activator Activated  relay",
            "rec_a User  relay",
            "rec_b User  relay",
            "rec_activator User  relay",
            "rec_a Class  relay",
            "rec_b Class  relay",
            "rec_activator Class  relay",
        ]
    );
}

#[test]
fn ent_fire_needs_cheats() {
    let g = player_game();
    let alice = g.runtime.join(alice()).unwrap();
    g.runtime.run_string("local r = ents.Create('logic_relay') r:SetName('door') r:Spawn() r:SetKeyValue('OnTrigger', '!activator,AddOutput,targetname opener,0,-1')", "relay").unwrap();
    g.runtime.run_player_command(alice, "ent_fire door Trigger");
    g.runtime.think(0.1);
    assert!(g.printed().contains("sv_cheats"));
    assert!(g.eval::<bool>("return Entity(1):GetName() == ''"));
    g.runtime.run_command("sv_cheats 1");
    g.runtime.run_player_command(alice, "ent_fire door Trigger");
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return Entity(1):GetName() == 'opener'"), "the player was the activator");
}
