//! AppleScript sources, embedded at compile time. User data is never
//! interpolated into these — inputs travel as osascript argv (TDD §4.1).

pub const SNAPSHOT: &str = include_str!("scripts/snapshot.scpt");
pub const IS_RUNNING: &str = include_str!("scripts/is_running.scpt");
pub const FRONT_WINDOW: &str = include_str!("scripts/front_window.scpt");
pub const SPAWN: &str = include_str!("scripts/spawn.scpt");
pub const ACTION: &str = include_str!("scripts/action.scpt");
