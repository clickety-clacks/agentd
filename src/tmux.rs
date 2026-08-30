use crate::model::TmuxLocation;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const FORMAT: &str = "#{n:pane_tty}:#{pane_tty}#{n:session_name}:#{session_name}#{n:window_index}:#{window_index}#{n:window_name}:#{window_name}#{n:pane_id}:#{pane_id}";
const DEADLINE: Duration = Duration::from_millis(250);
const STDOUT_LIMIT: usize = 1_048_576;

pub type TmuxIndex = HashMap<String, TmuxLocation>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseError;

pub trait TmuxSource: Send + Sync {
    fn index(&self) -> TmuxIndex;
}

#[derive(Debug, Default)]
pub struct NoTmux;

impl TmuxSource for NoTmux {
    fn index(&self) -> TmuxIndex {
        HashMap::new()
    }
}

#[derive(Debug, Default)]
pub struct SystemTmux;

impl TmuxSource for SystemTmux {
    fn index(&self) -> TmuxIndex {
        let Some(output) = run_tmux() else {
            return HashMap::new();
        };
        parse_index(&output).unwrap_or_default()
    }
}

fn run_tmux() -> Option<Vec<u8>> {
    run_tmux_program(OsStr::new("tmux"))
}

fn run_tmux_program(program: &OsStr) -> Option<Vec<u8>> {
    let mut child = Command::new(program)
        .args(["list-panes", "-a", "-F", FORMAT])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut output = Vec::new();
        let result = stdout
            .take((STDOUT_LIMIT + 1) as u64)
            .read_to_end(&mut output)
            .map(|_| output);
        let _ = sender.send(result);
    });

    let deadline = Instant::now() + DEADLINE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(2)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let remaining = deadline.checked_duration_since(Instant::now())?;
    let output = receiver.recv_timeout(remaining).ok()?.ok()?;
    (output.len() <= STDOUT_LIMIT).then_some(output)
}

pub fn parse_index(output: &[u8]) -> Result<TmuxIndex, ParseError> {
    std::str::from_utf8(output).map_err(|_| ParseError)?;
    let mut offset = 0;
    let mut rows = Vec::new();
    while offset < output.len() {
        let mut fields = Vec::with_capacity(5);
        for _ in 0..5 {
            let colon = output[offset..]
                .iter()
                .position(|byte| *byte == b':')
                .map(|relative| offset + relative)
                .ok_or(ParseError)?;
            let prefix = &output[offset..colon];
            if prefix.is_empty()
                || (prefix.len() > 1 && prefix[0] == b'0')
                || !prefix.iter().all(u8::is_ascii_digit)
            {
                return Err(ParseError);
            }
            let length = prefix.iter().try_fold(0_usize, |value, digit| {
                value.checked_mul(10)?.checked_add((digit - b'0') as usize)
            });
            let length = length.ok_or(ParseError)?;
            offset = colon.checked_add(1).ok_or(ParseError)?;
            let end = offset
                .checked_add(length)
                .filter(|end| *end <= output.len())
                .ok_or(ParseError)?;
            fields.push(std::str::from_utf8(&output[offset..end]).map_err(|_| ParseError)?);
            offset = end;
        }
        if output.get(offset) != Some(&b'\n') {
            return Err(ParseError);
        }
        offset += 1;
        rows.push(fields);
    }

    let mut index = HashMap::new();
    let mut ambiguous = HashSet::new();
    for fields in rows {
        let [pane_tty, session, window_index, window_name, pane_id] = fields.as_slice() else {
            unreachable!("five fields were parsed");
        };
        if session.is_empty()
            || window_name.is_empty()
            || session.chars().any(char::is_control)
            || window_name.chars().any(char::is_control)
            || !valid_pane_id(pane_id)
        {
            continue;
        }
        let Ok(window_index) = window_index.parse::<u32>() else {
            continue;
        };
        let Some(tty) = canonicalize_pane_tty(pane_tty) else {
            continue;
        };
        if ambiguous.contains(&tty) {
            continue;
        }
        let location = TmuxLocation {
            session: (*session).to_owned(),
            window_index,
            window_name: (*window_name).to_owned(),
            pane_id: (*pane_id).to_owned(),
        };
        if index.insert(tty.clone(), location).is_some() {
            index.remove(&tty);
            ambiguous.insert(tty);
        }
    }
    Ok(index)
}

fn valid_pane_id(value: &str) -> bool {
    value.strip_prefix('%').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn canonicalize_pane_tty(value: &str) -> Option<String> {
    if let Some(index) = value.strip_prefix("/dev/pts/") {
        let index: u32 = index.parse().ok()?;
        return Some(format!("pts/{index}"));
    }
    let path = Path::new(value);
    if !path.is_absolute() || !path.starts_with("/dev") {
        return None;
    }
    let metadata = fs::metadata(path).ok()?;
    if !metadata.file_type().is_char_device() {
        return None;
    }
    let device = metadata.rdev();
    let major = ((device >> 8) & 0x0fff) | ((device >> 32) & 0xffff_f000);
    let minor = (device & 0x00ff) | ((device >> 12) & 0xffff_ff00);
    Some(format!("dev/{major}:{minor}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(value: &str) -> String {
        format!("{}:{value}", value.len())
    }

    fn row(tty: &str, session: &str, index: &str, window: &str, pane: &str) -> Vec<u8> {
        format!(
            "{}{}{}{}{}\n",
            frame(tty),
            frame(session),
            frame(index),
            frame(window),
            frame(pane)
        )
        .into_bytes()
    }

    #[test]
    fn byte_lengths_preserve_colons_and_multibyte_utf8() {
        let output = row("/dev/pts/13", "agents", "2", "é:spec", "%7");
        let index = parse_index(&output).unwrap();
        let location = &index["pts/13"];
        assert_eq!(location.window_name, "é:spec");
        assert_eq!(location.window_index, 2);
    }

    #[test]
    fn captured_real_tmux_34_response_maps_both_panes() {
        let output = include_bytes!("../tests/fixtures/real-tmux-3.4-length-frame.txt");
        let index = parse_index(output).unwrap();
        assert_eq!(index.len(), 2);
        assert_eq!(index["pts/23"].session, "agents");
        assert_eq!(index["pts/23"].window_index, 0);
        assert_eq!(index["pts/23"].window_name, "é:spec");
        assert_eq!(index["pts/23"].pane_id, "%0");
        assert_eq!(index["pts/28"].pane_id, "%1");
    }

    #[test]
    fn controls_in_semantic_fields_drop_only_that_row() {
        let mut output = row("/dev/pts/13", "bad\nsession", "2", "spec", "%7");
        output.extend(row("/dev/pts/14", "agents", "3", "good", "%8"));
        let index = parse_index(&output).unwrap();
        assert!(!index.contains_key("pts/13"));
        assert!(index.contains_key("pts/14"));
    }

    #[test]
    fn duplicate_tty_is_ambiguous() {
        let mut output = row("/dev/pts/13", "one", "1", "a", "%1");
        output.extend(row("/dev/pts/13", "two", "2", "b", "%2"));
        assert!(!parse_index(&output).unwrap().contains_key("pts/13"));
    }

    #[test]
    fn malformed_frame_invalidates_the_complete_index() {
        let mut output = row("/dev/pts/13", "agents", "2", "spec", "%7");
        output.extend_from_slice(b"x:broken");
        assert_eq!(parse_index(&output), Err(ParseError));
        assert_eq!(parse_index(b"11:/dev/pts/13"), Err(ParseError));
        assert_eq!(parse_index(b"011:/dev/pts/13"), Err(ParseError));
    }

    #[test]
    fn invalid_utf8_invalidates_the_complete_index() {
        assert_eq!(parse_index(&[1, b':', 0xff, b'\n']), Err(ParseError));
    }

    #[test]
    fn absent_executable_and_nonzero_exit_fail_open() {
        assert_eq!(
            run_tmux_program(OsStr::new("/agentd-test/no-such-tmux")),
            None
        );
        assert_eq!(run_tmux_program(OsStr::new("/bin/false")), None);
    }
}
