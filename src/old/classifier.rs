use super::{ActiveWindowMetadata, ErrorMessage, UniqueCategories};
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::process;

/// Classifier: determines the category based on active window metadata.
pub trait Classifier {
    /// Returns the set of all categories defined in the classifier.
    fn categories(&self) -> UniqueCategories;

    /// Returns the category name for the metadata, or None if not matched.
    /// The category must be in the set returned by categories().
    fn classify(&mut self, metadata: ActiveWindowMetadata) -> Result<Option<String>, ErrorMessage>;
}

/** Classify using an external process.
 *
 * For each active window metadata change, the metadata is written on stdin of the subprocess.
 * Field values are tab separated on one line.
 * Undefined fields are mapped to empty string.
 * A first line with field names is outputed at initialization.
 *
 * The process must print the list of categories on stdout at startup.
 * Then for each line of metadata, print the category name on stdout.
 * An empty line is considered a None (no category), the time chunk will be ignored.
 */
pub struct Process {
    child: process::Child,
    stdout: BufReader<process::ChildStdout>,
    categories: UniqueCategories,
}

impl Process {
    /// Start a subprocess
    pub fn new<C, I, S>(command: C, args: I) -> Result<Self, ErrorMessage>
    where
        C: AsRef<OsStr>,
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let command_name = || command.as_ref().to_string_lossy();

        let mut child = process::Command::new(command.as_ref())
            .args(args)
            .stdin(process::Stdio::piped())
            .stdout(process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                ErrorMessage::new(format!("Cannot spawn process '{}'", command_name()), e)
            })?;
        // Send the field names (unbuffered!)
        Process::stdin(&mut child)
            .write_all(b"title\tclass\n")
            .map_err(|e| ErrorMessage::new("Process: cannot write to stdin", e))?;
        // Extract stdout from child instance to wrap it in bufreader.
        let stdout = child.stdout.take().unwrap();
        let mut stdout = BufReader::new(stdout);
        // Get category set from first line, tab separated.
        let categories = {
            let mut line = String::new();
            stdout
                .read_line(&mut line)
                .map_err(|e| ErrorMessage::new("Process: cannot read first line", e))?;
            if line.pop() != Some('\n') {
                return Err(ErrorMessage::from("Process: unexpected end of output"));
            }
            let categories: Vec<String> = line.split('\t').map(|s| s.into()).collect();
            UniqueCategories::from_unique(categories)
                .map_err(|e| ErrorMessage::new("Process: categories not unique", e))?
        };
        Ok(Process {
            child: child,
            stdout: stdout,
            categories: categories,
        })
    }

    fn stdin(child: &mut process::Child) -> &mut process::ChildStdin {
        // Stdin must have been piped by spawn, panic if not available.
        child.stdin.as_mut().expect("stdin undefined")
    }

    pub fn doc() -> &'static str {
        "Launch a process using the provided program name and arguments.\n\
         \n\
         On every update, the new window metadata is written to the process stdin.\n\
         Fields of metadata are on one line, tab separated.\n\
         Empty fields are encoded as empty strings (nothing between two tabs).\n\
         Each tab or newline in metadata field are converted to spaces.\n\
         The initial line sent to the process contains the field names, tab separated.\n\
         \n\
         The process must answer by writing lines to stdout.\n\
         It must write an initial line with all possible categories, tab separated.\n\
         For each metadata line, it must write a line containing the category name.\n\
         An empty line is interpreted as no category, and the duration will be ignored.\n\
         \n\
         IMPORTANT:\n\
         The classifier must output lines without buffering, or xstalker will be blocked.\n\
         Category names must not contain tabs or newlines."
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        // child.wait will close stdin to let the process terminate properly with EOF.
        self.child.wait().expect("Process: wait() failed");
    }
}
impl Classifier for Process {
    fn categories(&self) -> UniqueCategories {
        self.categories.clone()
    }
    fn classify(&mut self, metadata: ActiveWindowMetadata) -> Result<Option<String>, ErrorMessage> {
        let escape_field = |field: Option<String>| match field {
            Some(text) => text.replace(|c| c == '\t' || c == '\n', " "),
            None => String::new(),
        };
        let metadata = format!(
            "{}\t{}\n",
            escape_field(metadata.title),
            escape_field(metadata.class)
        );
        Process::stdin(&mut self.child)
            .write_all(metadata.as_bytes())
            .map_err(|e| ErrorMessage::new("Process: cannot write to stdin", e))?;
        // Receive category
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .map_err(|e| ErrorMessage::new("Process: cannot read reply line", e))?;
        if line.pop() != Some('\n') {
            return Err(ErrorMessage::from("Process: unexpected end of output"));
        }
        // Filter
        if line.is_empty() {
            Ok(None)
        } else if self.categories.contains(&line) {
            Ok(Some(line))
        } else {
            Err(ErrorMessage::from(format!(
                "Process: undeclared category '{}'",
                line
            )))
        }
    }
}

// TODO classifier with simple text matching rules ?
