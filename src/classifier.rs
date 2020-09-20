use crate::{ActiveWindowMetadata, TimeSpan};
use anyhow::{Context, Error};
use star::{set_nonblocking, FdEventType, WaitFdEvent};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, LineWriter, Write};
use std::os::unix::io::AsRawFd;
use std::process;
use std::rc::Rc;

struct Classifier {
    process: process::Child,
    awaiting_classification: RefCell<VecDeque<TimeSpan>>,
}

impl Drop for Classifier {
    fn drop(&mut self) {
        if let Err(e) = self.process.wait() {
            eprintln!("Failed to stop classifier process: {}", e)
        }
    }
}

pub struct ClassifierInput {
    // stdin placed first so that it is dropped before classifier.
    // This lets the process finish properly, so that the wait(pid) in Classifier::drop() completes.
    stdin: LineWriter<process::ChildStdin>,
    classifier: Rc<Classifier>,
}

pub struct ClassifierOutput {
    stdout: BufReader<process::ChildStdout>,
    classifier: Rc<Classifier>,
}

/// Spawn classifier subprocess.
///
/// For each time span, the metadata is given as a text line with the given format: ```"{id}\t{title}\t{class}\n"```.
/// Title and class have tabs or newlines replaced by spaces.
///
/// The classifier subprocess should return a line whose text (without newline) is considered the classification.
pub fn spawn(command: &Vec<String>) -> Result<(ClassifierInput, ClassifierOutput), Error> {
    let mut parts = command.iter();
    let command = parts
        .next()
        .ok_or_else(|| Error::msg("Empty classifier command line"))?;
    let mut process = process::Command::new(command)
        .args(parts)
        .stdin(process::Stdio::piped())
        .stdout(process::Stdio::piped())
        .spawn()
        .with_context(|| format!("Cannot spawn classifier process: '{}'", command))?;

    let stdin = process.stdin.take().unwrap();
    let stdout = process.stdout.take().unwrap();
    set_nonblocking(stdout.as_raw_fd())?;

    let classifier = Rc::new(Classifier {
        process,
        awaiting_classification: RefCell::new(VecDeque::new()),
    });
    Ok((
        ClassifierInput {
            stdin: LineWriter::new(stdin),
            classifier: classifier.clone(),
        },
        ClassifierOutput {
            stdout: BufReader::new(stdout),
            classifier,
        },
    ))
}

impl ClassifierInput {
    /// Send a time span + metadata for classification.
    ///
    /// For now this uses a blocking write.
    /// Blocking is unlikely unless the process does not process input fast enough.
    /// In that case blocking the whole stalker program is not problematic.
    pub fn classify(
        &mut self,
        metadata: &ActiveWindowMetadata,
        span: TimeSpan,
    ) -> Result<(), Error> {
        self.classifier
            .awaiting_classification
            .borrow_mut()
            .push_back(span);

        write!(
            &mut self.stdin,
            "{}\t{}\t{}\n",
            metadata.id,
            escape_opt_str(metadata.title.as_deref()),
            escape_opt_str(metadata.class.as_deref())
        )
        .with_context(|| "Failed to send metadata to classifier process")
    }
}

fn escape_opt_str<'s>(s: Option<&'s str>) -> Cow<'s, str> {
    const ESCAPED: &[char] = &['\t', '\n'];
    match s {
        Some(s) => match s.contains(ESCAPED) {
            true => Cow::Owned(s.replace(ESCAPED, " ")),
            false => Cow::Borrowed(&s),
        },
        None => Cow::Borrowed(""),
    }
}

impl ClassifierOutput {
    pub async fn classified(&mut self) -> Result<(String, TimeSpan), Error> {
        // Async-ly read a line
        let mut classification = String::new();
        loop {
            match self.stdout.read_line(&mut classification) {
                Ok(_) => break,
                Err(e) => match e.kind() {
                    io::ErrorKind::WouldBlock => {
                        WaitFdEvent::new(
                            self.stdout.get_ref().as_raw_fd(),
                            FdEventType::IN | FdEventType::ERR,
                        )
                        .await
                    }
                    _ => return Err(Error::new(e).context("Reading classification")),
                },
            }
        }

        // Post process classification
        if classification.is_empty() {
            return Err(Error::msg(
                "Classifier output was closed while waiting for classification",
            ));
        }
        let classification = classification.trim().to_string();

        let time_span = self
            .classifier
            .awaiting_classification
            .borrow_mut()
            .pop_front()
            .ok_or_else(|| Error::msg("Received classification while none was expected"))?;

        Ok((classification, time_span))
    }
}
