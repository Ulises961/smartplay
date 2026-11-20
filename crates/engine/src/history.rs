//! A user's history of plays on disk: one JSON line for each play, appended as it arrives.
//!
//! Appending keeps a play cheap however long the history is (the file is not read whole or rewritten for each play).
//! Plays may arrive late and out of order, so a reader sorts by time. Lines that cannot be read are skipped and never
//! dropped: only [`PlayLog::compact`] rewrites the file, and it keeps them as they were.

use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

use smartplay_core::plays::{self, Play};

/// A play told again arrives soon after the first time, so only the end of the file is searched for it: this many bytes,
/// some thousands of plays.
const TAIL_BYTES: u64 = 512 * 1024;

/// The history is rewritten only when its oldest play is this much older than the retention, so that a file is not rewritten
/// for each play that crosses the line.
const COMPACT_MARGIN: i64 = 30 * 24 * 60 * 60;

pub struct PlayLog {
    path: PathBuf,
    /// How much of the end of the file is searched for a play told again.
    tail_bytes: u64,
    /// Where the history was kept before it was a file of lines: one JSON list of the last plays.
    legacy: PathBuf,
}

impl PlayLog {
    /// `path` is the file of lines; the file the history had before it is looked for beside it, with the extension `json`.
    pub fn new(path: PathBuf) -> PlayLog {
        let legacy = path.with_extension("json");
        PlayLog {
            path,
            tail_bytes: TAIL_BYTES,
            legacy,
        }
    }

    /// Every play kept, oldest first.
    pub fn load(&self) -> Vec<Play> {
        self.migrate();
        let mut log: Vec<Play> = fs::read_to_string(&self.path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        log.sort_by_key(|p| p.at); // stable: plays of the same second keep the order they arrived in
        log
    }

    /// Adds a play, unless the same play is already there. Returns whether it was new.
    pub fn append(&self, play: &Play) -> Result<bool, String> {
        self.migrate();
        if self.tail().iter().any(|p| plays::same_play(p, play)) {
            return Ok(false);
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| e.to_string())?;
        // One write, so that two plays arriving at once cannot mix their lines. A file that does not end in a newline
        // (cut short) gets one first, or the new line would join the broken one.
        let mut line = serde_json::to_string(play).map_err(|e| e.to_string())?;
        line.push('\n');
        if !self.ends_in_newline() {
            line.insert(0, '\n');
        }
        file.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// Drops the plays older than `retention` seconds before `now`, when the oldest is far enough past it to be worth a
    /// rewrite. Lines that cannot be read are kept. Returns whether the file was rewritten.
    pub fn compact(&self, now: i64, retention: i64) -> Result<bool, String> {
        self.migrate();
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(_) => return Ok(false),
        };
        let oldest = text
            .lines()
            .filter_map(|l| serde_json::from_str::<Play>(l).ok())
            .map(|p| p.at)
            .min();
        if oldest.is_none_or(|at| at >= now - retention - COMPACT_MARGIN) {
            return Ok(false);
        }
        let mut kept = String::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let old = serde_json::from_str::<Play>(line).is_ok_and(|p| p.at < now - retention);
            if !old {
                kept.push_str(line);
                kept.push('\n');
            }
        }
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, kept).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &self.path).map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// The plays at the end of the file.
    fn tail(&self) -> Vec<Play> {
        let Ok(mut file) = fs::File::open(&self.path) else {
            return vec![];
        };
        let length = file.metadata().map(|m| m.len()).unwrap_or(0);
        let start = length.saturating_sub(self.tail_bytes);
        let mut bytes = Vec::new();
        if file.seek(SeekFrom::Start(start)).is_err() || file.read_to_end(&mut bytes).is_err() {
            return vec![];
        }
        let text = String::from_utf8_lossy(&bytes);
        // When the read began in the middle of the file its first line is a piece of a play.
        let skip = usize::from(start > 0);
        text.lines()
            .skip(skip)
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn ends_in_newline(&self) -> bool {
        let Ok(mut file) = fs::File::open(&self.path) else {
            return true;
        };
        let length = file.metadata().map(|m| m.len()).unwrap_or(0);
        if length == 0 {
            return true;
        }
        let mut last = [0u8; 1];
        file.seek(SeekFrom::Start(length - 1)).is_ok()
            && file.read_exact(&mut last).is_ok()
            && last[0] == b'\n'
    }

    /// Moves a history kept as one JSON list into the file of lines, once. A list that cannot be read is set aside, not
    /// written over.
    fn migrate(&self) {
        if !self.legacy.exists() || self.path.exists() {
            return;
        }
        let text = fs::read_to_string(&self.legacy).unwrap_or_default();
        let Ok(log) = serde_json::from_str::<Vec<Play>>(&text) else {
            let aside = self.legacy.with_extension("bad");
            eprintln!(
                "{}: unreadable; kept as {}",
                self.legacy.display(),
                aside.display()
            );
            let _ = fs::rename(&self.legacy, aside);
            return;
        };
        let lines: String = log
            .iter()
            .filter_map(|p| serde_json::to_string(p).ok())
            .map(|l| l + "\n")
            .collect();
        let tmp = self.path.with_extension("tmp");
        if fs::write(&tmp, lines).is_ok() && fs::rename(&tmp, &self.path).is_ok() {
            let _ = fs::remove_file(&self.legacy);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(name: &str) -> PlayLog {
        let dir = std::env::temp_dir().join(format!("smartplay-log-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        PlayLog::new(dir.join("plays.jsonl"))
    }

    fn play(song: &str, at: i64) -> Play {
        Play {
            song: song.into(),
            artists: vec!["A".into()],
            at,
            ..Play::default()
        }
    }

    #[test]
    fn plays_are_appended_and_read_back_in_time_order() {
        let log = log("order");
        assert!(log.append(&play("s2", 200)).unwrap());
        assert!(log.append(&play("s1", 100)).unwrap());
        let songs: Vec<String> = log.load().into_iter().map(|p| p.song).collect();
        assert_eq!(songs, ["s1", "s2"]);
    }

    #[test]
    fn a_play_told_twice_is_kept_once() {
        let log = log("twice");
        assert!(log.append(&play("s1", 100)).unwrap());
        assert!(!log.append(&play("s1", 103)).unwrap());
        assert_eq!(log.load().len(), 1);
    }

    #[test]
    fn a_line_that_cannot_be_read_is_skipped_and_kept() {
        let log = log("broken");
        fs::write(&log.path, "{ not a play\n").unwrap();
        assert!(log.append(&play("s1", 100)).unwrap());
        assert_eq!(log.load().len(), 1);
        assert!(fs::read_to_string(&log.path)
            .unwrap()
            .starts_with("{ not a play\n"));
        // And a file cut short in the middle of a line does not swallow the next play.
        fs::write(&log.path, r#"{"song":"s0","artists":["A"],"at":1"#).unwrap();
        assert!(log.append(&play("s9", 900)).unwrap());
        assert_eq!(log.load().len(), 1);
    }

    #[test]
    fn a_history_kept_as_one_list_moves_into_the_file_of_lines() {
        let log = log("legacy");
        fs::write(
            &log.legacy,
            serde_json::to_string(&[play("s1", 100), play("s2", 200)]).unwrap(),
        )
        .unwrap();
        assert_eq!(log.load().len(), 2);
        assert!(!log.legacy.exists());
        assert!(log.append(&play("s3", 300)).unwrap());
        assert_eq!(log.load().len(), 3);
    }

    #[test]
    fn a_list_that_cannot_be_read_is_set_aside() {
        let log = log("bad-legacy");
        fs::write(&log.legacy, "{ not a list").unwrap();
        assert!(log.load().is_empty());
        assert_eq!(
            fs::read_to_string(log.legacy.with_extension("bad")).unwrap(),
            "{ not a list"
        );
    }

    #[test]
    fn compaction_drops_old_plays_only_when_the_oldest_is_well_past_the_retention() {
        let log = log("compact");
        let retention = 1000;
        let now = 10 * COMPACT_MARGIN;
        log.append(&play("old", now - retention - 1)).unwrap();
        log.append(&play("new", now)).unwrap();
        assert!(
            !log.compact(now, retention).unwrap(),
            "one play over the line is not worth a rewrite"
        );
        assert_eq!(log.load().len(), 2);

        let log = self::log("compact-late");
        log.append(&play("old", now - retention - COMPACT_MARGIN - 1))
            .unwrap();
        log.append(&play("edge", now - retention)).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&log.path)
            .unwrap()
            .write_all(b"{ not a play\n")
            .unwrap();
        assert!(log.compact(now, retention).unwrap());
        let songs: Vec<String> = log.load().into_iter().map(|p| p.song).collect();
        assert_eq!(songs, ["edge"]);
        assert!(fs::read_to_string(&log.path)
            .unwrap()
            .contains("{ not a play"));
    }

    #[test]
    fn a_long_history_is_searched_only_at_its_end_for_a_repeat() {
        let mut log = log("long");
        log.tail_bytes = 4096;
        let lines: String = (0..600)
            .map(|i| {
                serde_json::to_string(&play(&format!("s{i}"), 1_000_000 + i * 100)).unwrap() + "\n"
            })
            .collect();
        fs::write(&log.path, lines).unwrap();
        assert!(!log.append(&play("s599", 1_000_000 + 599 * 100)).unwrap());
        assert!(
            log.append(&play("s0", 1_000_000)).unwrap(),
            "a repeat from far back is outside the part searched"
        );
        assert_eq!(log.load().len(), 601);
    }
}
