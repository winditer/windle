//! Process-spawning helpers.
//!
//! Windle is a GUI application, so on Windows it owns no console. Every
//! console program it starts would therefore be given a brand new console
//! window — one black flash per command — unless the child is asked to run
//! without one. Everything that shells out goes through [`hide_console`].

use std::process::Command;

/// `CREATE_NO_WINDOW`: run the child without allocating a console.
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Mark a command so Windows starts it without a console window. Without this
/// the child of a GUI process opens one of its own. No-op elsewhere.
pub fn hide_console(command: &mut Command) -> &mut Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    command
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flag has to survive onto the command so a child cannot open a
    /// console; on other platforms this is only about the helper building.
    #[test]
    fn hiding_the_console_keeps_the_command_usable() {
        let mut command = Command::new("echo");
        hide_console(&mut command);

        assert_eq!(command.get_program(), "echo");
    }
}
