//! Walking: the player moved tick by tick as the game moves them, and the eyes drawn between
//! the ticks.

use gmod_bsp::Collision;
use gmod_movement::{Command, FULL_MOVE, Player, Settings, buttons};

/// A tick: 66⅔ a second, as Garry's Mod runs by default.
/// A tick of the game's.
fn tick() -> f32 {
    gmod_lua::timer::tick_interval() as f32
}

/// The keys that move a player, held or not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Keys {
    pub forward: bool,
    pub back: bool,
    pub left: bool,
    pub right: bool,
    pub jump: bool,
    pub duck: bool,
    pub run: bool,
}

impl Keys {
    /// What they ask for, looking along `pitch` and `yaw`.
    pub fn command(&self, pitch: f32, yaw: f32) -> Command {
        let axis = |plus: bool, minus: bool| (f32::from(plus) - f32::from(minus)) * FULL_MOVE;
        let held = [(self.jump, buttons::JUMP), (self.duck, buttons::DUCK), (self.run, buttons::SPEED)];
        Command {
            angles: [pitch, yaw, 0.0],
            forward_move: axis(self.forward, self.back),
            side_move: axis(self.right, self.left),
            buttons: held.iter().filter(|(on, _)| *on).fold(0, |all, (_, button)| all | button),
        }
    }
}

/// A walking player, and where their eyes were a tick ago.
pub struct Walker {
    pub player: Player,
    last_eye: [f32; 3],
    /// Time not ticked yet.
    lag: f32,
}

impl Walker {
    pub fn new(player: Player) -> Walker {
        Walker { last_eye: player.eye(), player, lag: 0.0 }
    }

    /// Runs the ticks that `seconds` more make, and returns where to see from: between the
    /// last two ticks, so that the picture moves smoothly at any frame rate.
    pub fn advance(&mut self, world: &Collision, settings: &Settings, command: &Command, seconds: f32) -> [f32; 3] {
        self.lag += seconds;
        while self.lag >= tick() {
            self.last_eye = self.player.eye();
            self.player.tick(world, settings, command, tick());
            self.lag -= tick();
        }
        let (from, to) = (self.last_eye, self.player.eye());
        let t = self.lag / tick();
        std::array::from_fn(|i| from[i] + (to[i] - from[i]) * t)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gmod_bsp::Bsp;
    use gmod_bsp::testmap::{TestBrush, TestMap};

    use super::*;

    #[test]
    fn keys_make_commands() {
        let keys = Keys { forward: true, left: true, jump: true, run: true, ..Keys::default() };
        let command = keys.command(10.0, 90.0);
        assert_eq!(command.angles, [10.0, 90.0, 0.0]);
        assert_eq!((command.forward_move, command.side_move), (FULL_MOVE, -FULL_MOVE));
        assert_eq!(command.buttons, buttons::JUMP | buttons::SPEED);
        let both = Keys { forward: true, back: true, duck: true, ..Keys::default() }.command(0.0, 0.0);
        assert_eq!((both.forward_move, both.buttons), (0.0, buttons::DUCK));
    }

    #[test]
    fn ticks_and_draws_between_them() {
        let floor = TestMap { models: vec![vec![TestBrush::cuboid([-512.0, -512.0, -64.0], [512.0, 512.0, 0.0])]], ..TestMap::default() };
        let world = Collision::new(&Bsp::parse("test.bsp".into(), Arc::from(floor.bytes())).unwrap());
        let settings = Settings::default();
        let mut walker = Walker::new(Player::spawn(&world, &settings, [0.0, 0.0, 100.0]).unwrap());
        let still = Command::default();
        // less than a tick: nothing moves yet
        assert_eq!(walker.advance(&world, &settings, &still, 0.01), [0.0, 0.0, 164.0]);
        // two ticks and a third of one in all: a third of the way from the first tick's eyes
        // to the second's
        let eye = walker.advance(&world, &settings, &still, 0.025);
        let mut player = Player::spawn(&world, &settings, [0.0, 0.0, 100.0]).unwrap();
        let ticks: Vec<f32> = (0..2)
            .map(|_| {
                player.tick(&world, &settings, &still, tick());
                player.eye()[2]
            })
            .collect();
        assert!((eye[2] - (ticks[0] + (ticks[1] - ticks[0]) / 3.0)).abs() < 0.01, "{eye:?} {ticks:?}");
        // and down on the floor in the end
        assert!((walker.advance(&world, &settings, &still, 2.0)[2] - 64.0).abs() < 0.1);
        assert!(walker.player.on_ground);
    }
}
