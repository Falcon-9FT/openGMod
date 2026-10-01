//! Output that is printed and kept: the report of a check started from Explorer is also saved
//! to a file, to be read after the window has closed or sent along.

use std::io::Write;

#[derive(Debug, Default)]
pub struct Out {
    text: String,
}

impl Out {
    /// Prints a line and keeps it.
    pub fn line(&mut self, line: impl AsRef<str>) {
        let line = line.as_ref();
        // a reader that went away (`gmod-check | head`) is no reason to stop
        let _ = writeln!(std::io::stdout().lock(), "{line}");
        self.text.push_str(line);
        self.text.push('\n');
    }

    /// Everything printed so far.
    pub fn text(&self) -> &str {
        &self.text
    }
}
