use std::num::NonZeroUsize;
use std::path::Path;
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    #[error("parsing $EDITOR ({editor}): {reason}")]
    Unparsable {
        editor: String,
        reason: shell_words::ParseError,
    },
    #[error("$EDITOR is empty")]
    Empty,
    #[error("$EDITOR isn't valid UTF-8")]
    NotUnicode,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Editor {
    program: String,
    args: Vec<String>,
}

pub enum Target<'a> {
    Dir,
    File(&'a Path),
    Line(&'a Path, NonZeroUsize),
}

impl Editor {
    /// `$EDITOR`, or `nvim` when it isn't set.
    pub fn from_env() -> Result<Self, EditorError> {
        match std::env::var("EDITOR") {
            Ok(editor) => editor.parse(),
            Err(std::env::VarError::NotPresent) => "nvim".parse(),
            Err(std::env::VarError::NotUnicode(_)) => Err(EditorError::NotUnicode),
        }
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    /// A line is passed as `+N`, which vi-family editors understand.
    pub fn open(&self, cwd: &Path, target: &Target<'_>) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args).current_dir(cwd);
        match target {
            Target::Dir => {}
            Target::File(file) => {
                command.arg(file);
            }
            Target::Line(file, line) => {
                command.arg(format!("+{line}")).arg(file);
            }
        }
        command
    }
}

impl std::str::FromStr for Editor {
    type Err = EditorError;

    fn from_str(editor: &str) -> Result<Self, EditorError> {
        let argv = shell_words::split(editor).map_err(|reason| EditorError::Unparsable {
            editor: editor.to_string(),
            reason,
        })?;
        let mut argv = argv.into_iter();
        let program = argv.next().ok_or(EditorError::Empty)?;
        Ok(Self {
            program,
            args: argv.collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn argv(command: &Command) -> Vec<&OsStr> {
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .collect()
    }

    #[test]
    fn an_editor_keeps_its_arguments_before_the_target_and_runs_in_the_directory() {
        let editor: Editor = "nvim -u NONE".parse().unwrap();
        let cwd = Path::new("/repo");
        let file = Path::new("src/main.rs");
        let line = NonZeroUsize::new(12).unwrap();
        let dir = editor.open(cwd, &Target::Dir);
        assert_eq!(argv(&dir), ["nvim", "-u", "NONE"]);
        assert_eq!(dir.get_current_dir(), Some(cwd));
        assert_eq!(
            argv(&editor.open(cwd, &Target::File(file))),
            ["nvim", "-u", "NONE", "src/main.rs"]
        );
        assert_eq!(
            argv(&editor.open(cwd, &Target::Line(file, line))),
            ["nvim", "-u", "NONE", "+12", "src/main.rs"]
        );
    }

    #[test]
    fn an_empty_or_unbalanced_editor_is_refused() {
        assert!(matches!("  ".parse::<Editor>(), Err(EditorError::Empty)));
        assert!(matches!(
            "nvim 'oops".parse::<Editor>(),
            Err(EditorError::Unparsable { .. })
        ));
    }
}
