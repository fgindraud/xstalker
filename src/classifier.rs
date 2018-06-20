use super::{ActiveWindowMetadata, ErrorMessage, UniqueCategories};
use std;
use std::io::{BufRead, BufReader, Write};
use std::process;

/// Classifier: determines the category based on active window metadata.
pub trait Classifier {
    /// Returns the set of all categories defined in the classifier.
    fn categories(&self) -> UniqueCategories;

    /// Returns the category name for the metadata, or None if not matched.
    /// The category must be in the set returned by categories().
    fn classify(&mut self, metadata: &ActiveWindowMetadata)
        -> Result<Option<String>, ErrorMessage>;
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
pub struct ExternalProcess {
    child: process::Child,
    stdin: process::ChildStdin,
    stdout: BufReader<process::ChildStdout>,
    categories: UniqueCategories,
}

impl ExternalProcess {
    /// Start a subprocess
    pub fn new(program: &str) -> Result<Self, ErrorMessage> {
        let mut child = process::Command::new(program)
            .stdin(process::Stdio::piped())
            .stdout(process::Stdio::piped())
            .spawn()
            .map_err(|e| ErrorMessage::new(format!("Cannot start subprocess '{}'", program), e))?;
        // Extract piped IO descriptors
        let mut stdin =
            std::mem::replace(&mut child.stdin, None).expect("Child process must have stdin");
        let stdout =
            std::mem::replace(&mut child.stdout, None).expect("Child process must have stdout");
        // Send the field names
        stdin
            .write_all(b"title\tclass\n")
            .map_err(|e| ErrorMessage::new("Subprocess: cannot write to stdin", e))?;
        // Get category set from first line, tab separated.
        let mut stdout = BufReader::new(stdout);
        let categories = {
            let mut line = String::new();
            stdout
                .read_line(&mut line)
                .map_err(|e| ErrorMessage::new("Subprocess: cannot read first line", e))?;
            if line.pop() != Some('\n') {
                return Err(ErrorMessage::from("Subprocess: unexpected end of output"));
            }
            let categories: Vec<String> = line.split('\t').map(|s| s.into()).collect();
            UniqueCategories::from_unique(categories)
                .map_err(|e| ErrorMessage::new("Subprocess: categories not unique", e))?
        };
        Ok(ExternalProcess {
            child: child,
            stdin: stdin,
            stdout: stdout,
            categories: categories,
        })
    }
}
impl Drop for ExternalProcess {
    fn drop(&mut self) {
        // FIXME do something with return code ? should drop stdin then wait
        self.child.wait().expect("Child process wait() failed");
    }
}
impl Classifier for ExternalProcess {
    fn categories(&self) -> UniqueCategories {
        self.categories.clone()
    }
    fn classify(
        &mut self,
        metadata: &ActiveWindowMetadata,
    ) -> Result<Option<String>, ErrorMessage> {
        // Send metadata
        let metadata = format!(
            "{}\t{}\n",
            metadata.title.as_ref().map_or("", |s| s.as_str()),
            metadata.class.as_ref().map_or("", |s| s.as_str())
        );
        self.stdin
            .write_all(metadata.as_bytes())
            .map_err(|e| ErrorMessage::new("Subprocess: cannot write to stdin", e))?;
        // Receive category
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .map_err(|e| ErrorMessage::new("Subprocess: cannot read reply line", e))?;
        if line.pop() != Some('\n') {
            return Err(ErrorMessage::from("Subprocess: unexpected end of output"));
        }
        // Filter
        if line.is_empty() {
            Ok(None)
        } else if self.categories.contains(&line) {
            Ok(Some(line))
        } else {
            Err(ErrorMessage::from(format!(
                "Subprocess: undeclared category '{}'",
                line
            )))
        }
    }
}

/** TestClassifier: stores rules used to determine categories for time spent.
 * Rules are stored in an ordered list.
 * The first matching rule in the list chooses the category.
 * A category can appear in multiple rules.
 */
pub struct TestClassifier {
    filters: Vec<(String, Box<Fn(&ActiveWindowMetadata) -> bool>)>,
}
impl TestClassifier {
    /// Create a new classifier with no rules.
    pub fn new() -> Self {
        let mut classifier = TestClassifier {
            filters: Vec::new(),
        };
        classifier.append_filter(&"coding", |md| {
            md.class
                .as_ref()
                .map(|class| class == "konsole")
                .unwrap_or(false)
        });
        classifier.append_filter(&"unknown", |_| true);
        classifier
    }
    /// Add a rule at the end of the list, for the given category.
    fn append_filter<F>(&mut self, category: &str, filter: F)
    where
        F: 'static + Fn(&ActiveWindowMetadata) -> bool,
    {
        self.filters
            .push((String::from(category), Box::new(filter)));
    }
}
impl Classifier for TestClassifier {
    fn categories(&self) -> UniqueCategories {
        UniqueCategories::make_unique(
            self.filters
                .iter()
                .map(|(category, _)| category.clone())
                .collect(),
        )
    }

    fn classify(
        &mut self,
        metadata: &ActiveWindowMetadata,
    ) -> Result<Option<String>, ErrorMessage> {
        Ok(self.filters
            .iter()
            .find(|(_category, filter)| filter(metadata))
            .map(|(category, _filter)| category.clone()))
    }
}
