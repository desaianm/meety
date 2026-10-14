use anyhow::{bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone)]
pub struct Paths {
    pub root: PathBuf,
}
impl Paths {
    pub fn new(root: Option<PathBuf>) -> Result<Self> {
        let root = root
            .or_else(|| std::env::var_os("MEETLOCAL_HOME").map(PathBuf::from))
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                    .join("Library/Application Support/Meetlocal")
            });
        fs::create_dir_all(root.join("meetings"))?;
        fs::create_dir_all(root.join("models"))?;
        let paths = Self {
            root: fs::canonicalize(root)?,
        };
        paths.db()?;
        Ok(paths)
    }
    pub fn db_path(&self) -> PathBuf {
        self.root.join("meetlocal.sqlite3")
    }
    pub fn socket(&self) -> PathBuf {
        // Unix socket paths on macOS are limited to 104 bytes.
        use sha2::{Digest, Sha256};
        let hash = format!(
            "{:x}",
            Sha256::digest(self.root.as_os_str().as_encoded_bytes())
        );
        std::env::temp_dir().join(format!("meetlocal-{}.sock", &hash[..16]))
    }
    pub fn meeting_dir(&self, id: &str) -> PathBuf {
        self.root.join("meetings").join(id)
    }
    pub fn db(&self) -> Result<Connection> {
        let conn = Connection::open(self.db_path())?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS meetings (
                id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at TEXT NOT NULL,
                status TEXT NOT NULL, error TEXT, duration_seconds REAL NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS segments (
                id INTEGER PRIMARY KEY, meeting_id TEXT NOT NULL REFERENCES meetings(id),
                start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL, source TEXT NOT NULL, text TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS segments_meeting ON segments(meeting_id,start_ms);
            CREATE VIRTUAL TABLE IF NOT EXISTS transcript_search USING fts5(meeting_id UNINDEXED, text);")?;
        let columns = conn
            .prepare("PRAGMA table_info(meetings)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !columns.iter().any(|name| name == "notes") {
            conn.execute(
                "ALTER TABLE meetings ADD COLUMN notes TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        // Existing meetings always retain their titles, even old placeholders.
        // Ownership is explicit for every meeting created after this migration.
        if !columns.iter().any(|name| name == "title_origin") {
            conn.execute(
                "ALTER TABLE meetings ADD COLUMN title_origin TEXT NOT NULL DEFAULT 'user'",
                [],
            )?;
        }
        if !columns.iter().any(|name| name == "title_revision") {
            conn.execute(
                "ALTER TABLE meetings ADD COLUMN title_revision INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        Ok(conn)
    }
    pub fn create(&self, title: &str, status: &str) -> Result<String> {
        self.create_with_origin(title, status, "user")
    }
    /// Explicitly opt a newly created placeholder into automatic titling.
    pub fn create_untitled(&self, title: &str, status: &str) -> Result<String> {
        self.create_with_origin(title, status, "automatic")
    }
    fn create_with_origin(&self, title: &str, status: &str, origin: &str) -> Result<String> {
        if title.trim().is_empty() || title.len() > 500 {
            bail!("Title must contain 1–500 bytes");
        }
        let id = uuid::Uuid::new_v4().to_string();
        fs::create_dir(self.meeting_dir(&id))?;
        self.db()?.execute(
            "INSERT INTO meetings(id,title,created_at,status,title_origin) VALUES(?1,?2,?3,?4,?5)",
            params![id, title.trim(), Utc::now().to_rfc3339(), status, origin],
        )?;
        Ok(id)
    }
    pub fn set_status(&self, id: &str, status: &str, error: Option<&str>) -> Result<()> {
        self.db()?.execute(
            "UPDATE meetings SET status=?2,error=?3 WHERE id=?1",
            params![id, status, error],
        )?;
        Ok(())
    }
    pub fn list(&self, limit: usize) -> Result<Vec<Meeting>> {
        let conn = self.db()?;
        let mut stmt = conn.prepare("SELECT id,title,created_at,status,error,duration_seconds,title_origin,title_revision FROM meetings ORDER BY created_at DESC LIMIT ?1")?;
        let rows = stmt.query_map([limit.clamp(1, 500)], meeting_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn resolve(&self, id: &str) -> Result<String> {
        if id == "latest" {
            return self
                .list(1)?
                .first()
                .map(|m| m.id.clone())
                .context("No meetings yet");
        }
        // IDs double as directory names, so reject traversal before using one.
        let parsed = uuid::Uuid::parse_str(id).context("Use a full meeting UUID or 'latest'")?;
        Ok(parsed.to_string())
    }
    pub fn get(&self, id: &str) -> Result<Transcript> {
        let id = self.resolve(id)?;
        let conn = self.db()?;
        let meeting = conn.query_row("SELECT id,title,created_at,status,error,duration_seconds,title_origin,title_revision FROM meetings WHERE id=?1", [&id], meeting_from_row).context("Meeting not found")?;
        let mut stmt = conn.prepare("SELECT start_ms,end_ms,source,text FROM segments WHERE meeting_id=?1 ORDER BY start_ms,id")?;
        let segments = stmt
            .query_map([&id], |row| {
                Ok(Segment {
                    start_ms: row.get(0)?,
                    end_ms: row.get(1)?,
                    source: row.get(2)?,
                    text: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let notes = conn.query_row("SELECT notes FROM meetings WHERE id=?1", [&id], |r| {
            r.get(0)
        })?;
        Ok(Transcript {
            meeting,
            segments,
            notes,
        })
    }
    pub fn save_notes(&self, id: &str, title: &str, notes: &str) -> Result<()> {
        // Legacy callers supply a title intentionally; protect it conservatively.
        self.save_notes_with_title_edit(id, title, notes, true)
    }
    /// `title_edited` records user intent, including renaming to the same text.
    /// Notes-only saves never write the supplied title: it may be stale while
    /// automatic generation finishes in another thread.
    pub fn save_notes_with_title_edit(
        &self,
        id: &str,
        title: &str,
        notes: &str,
        title_edited: bool,
    ) -> Result<()> {
        let id = self.resolve(id)?;
        if title.trim().is_empty() || title.len() > 500 {
            bail!("Title must contain 1–500 bytes");
        }
        if notes.len() > 1_048_576 {
            bail!("Notes are limited to 1 MB");
        }
        let changed = self.db()?.execute(
            "UPDATE meetings SET
                title_origin=CASE WHEN ?4 THEN 'user' ELSE title_origin END,
                title_revision=title_revision+CASE WHEN ?4 THEN 1 ELSE 0 END,
                title=CASE WHEN ?4 THEN ?2 ELSE title END,notes=?3 WHERE id=?1",
            params![id, title.trim(), notes, title_edited],
        )?;
        if changed == 0 {
            bail!("Meeting not found");
        }
        self.write_markdown(&id)?;
        Ok(())
    }
    /// Apply an automatic title only if no title edit happened since generation began.
    pub fn apply_automatic_title(
        &self,
        id: &str,
        expected_revision: i64,
        title: &str,
        origin: &str,
    ) -> Result<bool> {
        let id = self.resolve(id)?;
        if title.trim().is_empty()
            || title.len() > 500
            || !matches!(origin, "on_device_model" | "transcript_excerpt")
        {
            bail!("Invalid automatic title");
        }
        let changed = self.db()?.execute(
            "UPDATE meetings SET title=?3,title_origin=?4,title_revision=title_revision+1
             WHERE id=?1 AND title_origin='automatic' AND title_revision=?2",
            params![id, expected_revision, title.trim(), origin],
        )?;
        if changed != 0 {
            self.write_markdown(&id)?;
        }
        Ok(changed != 0)
    }
    pub fn save_transcript(&self, id: &str, segments: &[Segment], duration: f64) -> Result<()> {
        let mut conn = self.db()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM segments WHERE meeting_id=?1", [id])?;
        tx.execute("DELETE FROM transcript_search WHERE meeting_id=?1", [id])?;
        for s in segments {
            tx.execute("INSERT INTO segments(meeting_id,start_ms,end_ms,source,text) VALUES(?1,?2,?3,?4,?5)",params![id,s.start_ms,s.end_ms,s.source,s.text])?;
            tx.execute(
                "INSERT INTO transcript_search(meeting_id,text) VALUES(?1,?2)",
                params![id, s.text],
            )?;
        }
        tx.execute(
            "UPDATE meetings SET status='complete',error=NULL,duration_seconds=?2 WHERE id=?1",
            params![id, duration],
        )?;
        tx.commit()?;
        self.write_markdown(id)?;
        Ok(())
    }
    fn write_markdown(&self, id: &str) -> Result<()> {
        use fs2::FileExt;
        // Serialize read+export across notes and title writers. Otherwise an
        // older title snapshot could replace a newer user's Markdown export.
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.meeting_dir(id).join("markdown.lock"))?;
        lock.lock_exclusive()?;
        atomic_write(
            &self.meeting_dir(id).join("transcript.md"),
            self.get(id)?.markdown().as_bytes(),
        )
    }
    pub fn delete(&self, id: &str) -> Result<()> {
        use fs2::FileExt;
        let id = self.resolve(id)?;
        // Coordinate with imports/retries in other CLI processes as well as
        // the app's worker. Never erase files while a transcriber uses them.
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join("transcription.lock"))?;
        lock.try_lock_exclusive()
            .context("Wait for transcription to finish before deleting a meeting")?;
        let mut conn = self.db()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let status: String = tx
            .query_row("SELECT status FROM meetings WHERE id=?1", [&id], |r| {
                r.get(0)
            })
            .context("Meeting not found")?;
        if matches!(status.as_str(), "recording" | "ready" | "transcribing") {
            bail!("Wait for recording or transcription to finish before deleting this meeting");
        }
        tx.execute("DELETE FROM transcript_search WHERE meeting_id=?1", [&id])?;
        tx.execute("DELETE FROM segments WHERE meeting_id=?1", [&id])?;
        tx.execute("DELETE FROM meetings WHERE id=?1", [&id])?;
        let directory = self.meeting_dir(&id);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_symlink() => fs::remove_file(&directory)?,
            Ok(_) => fs::remove_dir_all(&directory)
                .context("Could not remove the meeting's saved files")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        tx.commit()?;
        Ok(())
    }
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<serde_json::Value>> {
        let terms: Vec<_> = query
            .split_whitespace()
            .take(30)
            .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
            .collect();
        if terms.is_empty() {
            return Ok(vec![]);
        }
        let conn = self.db()?;
        let mut stmt = conn.prepare("SELECT m.id,m.title,m.created_at,s.text FROM transcript_search s JOIN meetings m ON m.id=s.meeting_id WHERE transcript_search MATCH ?1 ORDER BY rank LIMIT ?2")?;
        let matches = stmt.query_map(params![terms.join(" AND "),limit.clamp(1,100)],|r|Ok(serde_json::json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"created_at":r.get::<_,String>(2)?,"text":r.get::<_,String>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(matches)
    }
    pub fn recover(&self) -> Result<()> {
        self.db()?.execute("UPDATE meetings SET status='interrupted',error='App exited before completion. Original audio is retained; run meetlocal retry ID.' WHERE status IN ('recording','transcribing')",[])?;
        Ok(())
    }
}

fn meeting_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Meeting> {
    Ok(Meeting {
        id: row.get(0)?,
        title: row.get(1)?,
        created_at: row.get(2)?,
        status: row.get(3)?,
        error: row.get(4)?,
        duration_seconds: row.get(5)?,
        title_origin: row.get(6)?,
        title_revision: row.get(7)?,
    })
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meeting {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub status: String,
    pub error: Option<String>,
    pub duration_seconds: f64,
    /// user, automatic (placeholder), on_device_model, or transcript_excerpt.
    pub title_origin: String,
    pub title_revision: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Segment {
    pub start_ms: i64,
    pub end_ms: i64,
    pub source: String,
    pub text: String,
}
#[derive(Debug, Serialize)]
pub struct Transcript {
    pub meeting: Meeting,
    pub segments: Vec<Segment>,
    pub notes: String,
}
impl Transcript {
    pub fn markdown(&self) -> String {
        let mut text = format!(
            "# {}\n\nDate: {}\nStatus: {}\n\n",
            self.meeting.title, self.meeting.created_at, self.meeting.status
        );
        if !self.notes.trim().is_empty() {
            text.push_str(&format!("## Notes\n\n{}\n\n", self.notes.trim()));
        }
        text.push_str("## Transcript\n\n");
        for s in &self.segments {
            let sec = s.start_ms / 1000;
            text.push_str(&format!(
                "**[{:02}:{:02}:{:02}] {}:** {}\n\n",
                sec / 3600,
                (sec / 60) % 60,
                sec % 60,
                s.source,
                s.text.trim()
            ));
        }
        text
    }
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_title_migration_protects_all_existing_titles() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let db = Connection::open(temp.path().join("meetlocal.sqlite3"))?;
        db.execute_batch("CREATE TABLE meetings(id TEXT PRIMARY KEY,title TEXT NOT NULL,created_at TEXT NOT NULL,status TEXT NOT NULL,error TEXT,duration_seconds REAL NOT NULL DEFAULT 0);
            INSERT INTO meetings(id,title,created_at,status) VALUES('legacy','Untitled meeting','2026-01-01','complete');")?;
        drop(db);
        let p = Paths::new(Some(temp.path().into()))?;
        let meeting = p.list(1)?.remove(0);
        assert_eq!(meeting.title, "Untitled meeting");
        assert_eq!(meeting.title_origin, "user");
        assert_eq!(meeting.title_revision, 0);
        Ok(())
    }
    #[test]
    fn automatic_title_persists_and_stale_notes_save_does_not_rename_it() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let p = Paths::new(Some(temp.path().into()))?;
        let id = p.create_untitled("Untitled meeting", "complete")?;
        p.save_notes_with_title_edit(&id, "Untitled meeting", "Draft notes", false)?;
        assert!(p.apply_automatic_title(
            &id,
            0,
            "Customer Onboarding Review",
            "on_device_model"
        )?);
        p.save_notes_with_title_edit(&id, "Untitled meeting", "Later notes", false)?;
        let reopened = Paths::new(Some(temp.path().into()))?;
        let saved = reopened.get(&id)?;
        assert_eq!(saved.meeting.title, "Customer Onboarding Review");
        assert_eq!(saved.meeting.title_origin, "on_device_model");
        assert_eq!(saved.notes, "Later notes");
        assert!(saved.markdown().starts_with("# Customer Onboarding Review"));
        assert!(!p.apply_automatic_title(&id, 0, "Outdated Title", "transcript_excerpt")?);
        Ok(())
    }
    #[test]
    fn deleting_meeting_removes_notes_search_transcript_and_audio() -> Result<()> {
        let t = tempfile::tempdir()?;
        let p = Paths::new(Some(t.path().into()))?;
        let id = p.create("Delete fixture", "draft")?;
        let keep = p.create("Keep fixture", "draft")?;
        p.save_notes(&id, "Delete fixture", "Private notes")?;
        p.save_transcript(
            &id,
            &[Segment {
                start_ms: 0,
                end_ms: 1000,
                source: "You".into(),
                text: "uniqueword".into(),
            }],
            1.0,
        )?;
        for file in ["microphone.wav", "system.wav", "import.wav"] {
            fs::write(p.meeting_dir(&id).join(file), b"fixture")?;
        }
        p.delete(&id)?;
        assert!(p.get(&id).is_err());
        assert!(p.search("uniqueword", 20)?.is_empty());
        assert!(!p.meeting_dir(&id).exists());
        assert!(p.get(&keep).is_ok());
        for table in ["meetings", "segments", "transcript_search"] {
            let count: i64 = p.db()?.query_row(
                &format!(
                    "SELECT count(*) FROM {table} WHERE {}=?1",
                    if table == "meetings" {
                        "id"
                    } else {
                        "meeting_id"
                    }
                ),
                [&id],
                |r| r.get(0),
            )?;
            assert_eq!(count, 0);
        }
        Ok(())
    }
    #[test]
    fn deletion_rejects_busy_meetings_and_unsafe_ids() -> Result<()> {
        use fs2::FileExt;
        let t = tempfile::tempdir()?;
        let p = Paths::new(Some(t.path().into()))?;
        for status in ["recording", "ready", "transcribing"] {
            let id = p.create("Busy", status)?;
            assert!(p.delete(&id).is_err());
            assert!(p.get(&id).is_ok());
        }
        assert!(p.delete("../../models").is_err());
        assert!(p.root.join("models").is_dir());
        let id = p.create("Locked", "draft")?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(p.root.join("transcription.lock"))?;
        lock.lock_exclusive()?;
        assert!(p.delete(&id).is_err());
        assert!(p.get(&id).is_ok());
        Ok(())
    }
    #[test]
    fn replace_transcript_is_atomic_and_searchable() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let p = Paths::new(Some(temp.path().into()))?;
        let id = p.create("Planning", "transcribing")?;
        let s = |text: &str| Segment {
            start_ms: 1000,
            end_ms: 2000,
            source: "You".into(),
            text: text.into(),
        };
        p.save_transcript(&id, &[s("launch on Friday")], 3.0)?;
        assert_eq!(p.search("Friday", 20)?.len(), 1);
        p.save_transcript(&id, &[s("launch on Monday")], 3.0)?;
        assert!(p.search("Friday", 20)?.is_empty());
        assert_eq!(p.search("Monday", 20)?.len(), 1);
        assert_eq!(p.get("latest")?.meeting.status, "complete");
        assert!(p.resolve("../../outside").is_err());
        Ok(())
    }
    #[test]
    fn recovery_preserves_audio_and_marks_unfinished() -> Result<()> {
        let t = tempfile::tempdir()?;
        let p = Paths::new(Some(t.path().into()))?;
        let id = p.create("Unfinished", "recording")?;
        fs::write(p.meeting_dir(&id).join("microphone.wav"), b"sample")?;
        p.recover()?;
        assert_eq!(p.get(&id)?.meeting.status, "interrupted");
        assert!(p.meeting_dir(&id).join("microphone.wav").exists());
        Ok(())
    }
    #[test]
    fn typed_notes_survive_transcription_and_markdown_export() -> Result<()> {
        let t = tempfile::tempdir()?;
        let p = Paths::new(Some(t.path().into()))?;
        let id = p.create("Untitled meeting", "draft")?;
        p.save_notes(&id, "Planning", "Decide the launch date")?;
        p.save_transcript(
            &id,
            &[Segment {
                start_ms: 0,
                end_ms: 1000,
                source: "You".into(),
                text: "Friday works".into(),
            }],
            1.0,
        )?;
        let transcript = p.get(&id)?;
        assert_eq!(transcript.notes, "Decide the launch date");
        assert_eq!(transcript.meeting.title, "Planning");
        let markdown = fs::read_to_string(p.meeting_dir(&id).join("transcript.md"))?;
        assert!(markdown.contains("## Notes\n\nDecide the launch date"));
        assert!(markdown.contains("Friday works"));
        Ok(())
    }
}
