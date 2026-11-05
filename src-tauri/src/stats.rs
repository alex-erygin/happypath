use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_DURATION_SECONDS: u64 = 20 * 60;
static NEXT_SESSION_SUFFIX: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TimerStatus {
    Idle,
    Running,
    Paused,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub status: TimerStatus,
    pub session_id: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub deadline_at: Option<DateTime<Utc>>,
    pub remaining_seconds: u64,
    pub duration_seconds: u64,
}

impl SessionState {
    fn idle(duration_seconds: u64) -> Self {
        Self {
            status: TimerStatus::Idle,
            session_id: None,
            started_at: None,
            deadline_at: None,
            remaining_seconds: duration_seconds,
            duration_seconds,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedSession {
    pub session_id: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub local_date: NaiveDate,
    pub focused_seconds: u64,
    pub duration_seconds: u64,
    pub app_version: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub status: TimerStatus,
    pub remaining_seconds: u64,
    pub elapsed_seconds: u64,
    pub duration_seconds: u64,
    pub progress: f64,
    pub week: [bool; 5],
    pub today_completed: bool,
    pub total_sessions: usize,
    pub total_seconds: u64,
}

pub struct StatsStore {
    data_dir: PathBuf,
    state: SessionState,
    sessions: Vec<CompletedSession>,
    completed_ids: HashSet<String>,
}

pub fn data_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("HAPPYPATH_DATA_DIR") {
        return PathBuf::from(path);
    }
    if let Some(path) = std::env::var_os("APPDATA") {
        return PathBuf::from(path).join("HappyPath");
    }
    if let Some(path) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(path).join(".happypath");
    }
    PathBuf::from("happypath-data")
}

/// Явный отказ пользователя от автозапуска сохраняется между запусками.
pub fn autostart_disabled(data_dir: &Path) -> bool {
    fs::read_to_string(data_dir.join("config.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.get("autostart_off")?.as_bool())
        .unwrap_or(false)
}

pub fn set_autostart_disabled(data_dir: &Path, disabled: bool) -> io::Result<()> {
    fs::create_dir_all(data_dir)?;
    atomic_write(
        &data_dir.join("config.json"),
        json!({ "autostart_off": disabled }).to_string().as_bytes(),
    )
}

pub fn configured_duration() -> u64 {
    let test_mode = std::env::var_os("HAPPYPATH_DATA_DIR").is_some();
    let fallback = DEFAULT_DURATION_SECONDS;
    let requested = std::env::var("HAPPYPATH_DURATION_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(fallback);
    // Короткие сессии нужны только изолированным тестам. В обычном запуске
    // случайной переменной окружения нельзя превратить 20 минут в 1 секунду.
    if test_mode {
        requested.clamp(1, 24 * 60 * 60)
    } else {
        requested.clamp(60, 24 * 60 * 60)
    }
}

impl StatsStore {
    pub fn load() -> io::Result<Self> {
        Self::load_from(data_dir(), configured_duration())
    }

    pub fn load_from(data_dir: PathBuf, duration_seconds: u64) -> io::Result<Self> {
        fs::create_dir_all(&data_dir)?;
        let events_path = data_dir.join("events.jsonl");
        let mut sessions = Vec::new();
        let mut completed_ids = HashSet::new();

        if events_path.exists() {
            for (line_number, line) in fs::read_to_string(&events_path)?.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let value = match serde_json::from_str::<Value>(line) {
                    Ok(value) => value,
                    Err(error) => {
                        eprintln!(
                            "HappyPath: пропущена повреждённая строка events.jsonl #{}: {}",
                            line_number + 1,
                            error
                        );
                        continue;
                    }
                };
                if value.get("event").and_then(Value::as_str) != Some("session_completed") {
                    continue;
                }
                match serde_json::from_value::<CompletedSession>(value) {
                    Ok(session) => {
                        // Дедупликация нужна после аварии между append журнала и
                        // сохранением state.json: повторный запуск не удвоит статистику.
                        if completed_ids.insert(session.session_id.clone()) {
                            sessions.push(session);
                        }
                    }
                    Err(error) => eprintln!(
                        "HappyPath: некорректная session_completed в строке #{}: {}",
                        line_number + 1,
                        error
                    ),
                }
            }
        }

        let state_path = data_dir.join("state.json");
        let state = match fs::read(&state_path) {
            Ok(bytes) => match serde_json::from_slice::<SessionState>(&bytes) {
                Ok(state) => state,
                Err(error) => {
                    eprintln!("HappyPath: повреждён state.json: {error}");
                    let backup = data_dir.join(format!(
                        "state.corrupt-{}.json",
                        Utc::now().timestamp_millis()
                    ));
                    let _ = fs::rename(&state_path, backup);
                    SessionState::idle(duration_seconds)
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                SessionState::idle(duration_seconds)
            }
            Err(error) => return Err(error),
        };

        Ok(Self {
            data_dir,
            state,
            sessions,
            completed_ids,
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn csv_path(&self) -> PathBuf {
        self.data_dir.join("sessions.csv")
    }

    pub fn snapshot_at(&mut self, now: DateTime<Utc>) -> io::Result<Snapshot> {
        self.refresh_at(now)?;
        Ok(self.snapshot_without_refresh(now))
    }

    fn snapshot_without_refresh(&self, now: DateTime<Utc>) -> Snapshot {
        let remaining_seconds = self.remaining_at(now);
        let elapsed_seconds = self
            .state
            .duration_seconds
            .saturating_sub(remaining_seconds);
        let today = local_date(now);
        let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64);
        let mut week = [false; 5];
        for (index, day) in week.iter_mut().enumerate() {
            let date = monday + Duration::days(index as i64);
            *day = self
                .sessions
                .iter()
                .any(|session| session.local_date == date);
        }

        Snapshot {
            status: self.state.status,
            remaining_seconds,
            elapsed_seconds,
            duration_seconds: self.state.duration_seconds,
            progress: if self.state.duration_seconds == 0 {
                0.0
            } else {
                elapsed_seconds as f64 / self.state.duration_seconds as f64
            },
            week,
            today_completed: self
                .sessions
                .iter()
                .any(|session| session.local_date == today),
            total_sessions: self.sessions.len(),
            total_seconds: self
                .sessions
                .iter()
                .map(|session| session.focused_seconds)
                .sum(),
        }
    }

    pub fn toggle_at(&mut self, now: DateTime<Utc>) -> io::Result<Snapshot> {
        self.refresh_at(now)?;
        match self.state.status {
            TimerStatus::Idle | TimerStatus::Completed => self.start_at(now)?,
            TimerStatus::Running => self.pause_at(now)?,
            TimerStatus::Paused => self.resume_at(now)?,
        }
        Ok(self.snapshot_without_refresh(now))
    }

    pub fn reset_at(&mut self, now: DateTime<Utc>) -> io::Result<Snapshot> {
        self.refresh_at(now)?;
        if let Some(session_id) = self.state.session_id.clone() {
            let remaining_seconds = self.remaining_at(now);
            self.append_event(
                "session_reset",
                now,
                json!({
                    "session_id": session_id,
                    "remaining_seconds": remaining_seconds,
                    "elapsed_seconds": self.state.duration_seconds.saturating_sub(remaining_seconds),
                    "previous_status": self.state.status,
                }),
            )?;
        }
        let duration = self.state.duration_seconds;
        self.state = SessionState::idle(duration);
        self.save_state()?;
        Ok(self.snapshot_without_refresh(now))
    }

    pub fn export_csv(&self) -> io::Result<PathBuf> {
        let mut output = String::from("\u{feff}");
        output.push_str(
            "session_id,local_date,weekday,started_at_utc,completed_at_utc,focused_seconds,focused_minutes,duration_seconds,app_version\n",
        );
        let mut sessions = self.sessions.clone();
        sessions.sort_by_key(|session| session.completed_at);
        for session in sessions {
            writeln!(
                output,
                "{},{},{},{},{},{},{:.2},{},{}",
                session.session_id,
                session.local_date,
                session.local_date.weekday().number_from_monday(),
                rfc3339(session.started_at),
                rfc3339(session.completed_at),
                session.focused_seconds,
                session.focused_seconds as f64 / 60.0,
                session.duration_seconds,
                session.app_version,
            )
            .expect("writing to String cannot fail");
        }
        let path = self.csv_path();
        atomic_write(&path, output.as_bytes())?;
        Ok(path)
    }

    fn start_at(&mut self, now: DateTime<Utc>) -> io::Result<()> {
        let duration_seconds = self.state.duration_seconds.max(1);
        let suffix = NEXT_SESSION_SUFFIX.fetch_add(1, Ordering::Relaxed);
        let session_id = format!(
            "{:x}-{}-{}",
            now.timestamp_micros(),
            std::process::id(),
            suffix
        );
        let deadline_at = now + Duration::seconds(duration_seconds as i64);
        self.append_event(
            "session_started",
            now,
            json!({
                "session_id": session_id,
                "started_at": rfc3339(now),
                "local_date": local_date(now),
                "duration_seconds": duration_seconds,
            }),
        )?;
        self.state = SessionState {
            status: TimerStatus::Running,
            session_id: Some(session_id),
            started_at: Some(now),
            deadline_at: Some(deadline_at),
            remaining_seconds: duration_seconds,
            duration_seconds,
        };
        self.save_state()
    }

    fn pause_at(&mut self, now: DateTime<Utc>) -> io::Result<()> {
        let remaining_seconds = self.remaining_at(now);
        self.append_event(
            "session_paused",
            now,
            json!({
                "session_id": self.state.session_id,
                "remaining_seconds": remaining_seconds,
                "elapsed_seconds": self.state.duration_seconds.saturating_sub(remaining_seconds),
            }),
        )?;
        self.state.status = TimerStatus::Paused;
        self.state.deadline_at = None;
        self.state.remaining_seconds = remaining_seconds;
        self.save_state()
    }

    fn resume_at(&mut self, now: DateTime<Utc>) -> io::Result<()> {
        let deadline_at = now + Duration::seconds(self.state.remaining_seconds as i64);
        self.append_event(
            "session_resumed",
            now,
            json!({
                "session_id": self.state.session_id,
                "remaining_seconds": self.state.remaining_seconds,
                "deadline_at": rfc3339(deadline_at),
            }),
        )?;
        self.state.status = TimerStatus::Running;
        self.state.deadline_at = Some(deadline_at);
        self.save_state()
    }

    fn refresh_at(&mut self, now: DateTime<Utc>) -> io::Result<()> {
        if self.state.status != TimerStatus::Running {
            return Ok(());
        }
        let Some(deadline_at) = self.state.deadline_at else {
            return Ok(());
        };
        if now < deadline_at {
            return Ok(());
        }

        let session_id = self.state.session_id.clone().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "running session has no id")
        })?;
        if !self.completed_ids.contains(&session_id) {
            let started_at = self.state.started_at.unwrap_or(deadline_at);
            let session = CompletedSession {
                session_id: session_id.clone(),
                started_at,
                completed_at: deadline_at,
                local_date: local_date(deadline_at),
                focused_seconds: self.state.duration_seconds,
                duration_seconds: self.state.duration_seconds,
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            };
            let mut event = serde_json::to_value(&session)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            event["recorded_at"] = Value::String(rfc3339(now));
            // Сначала журнал, затем состояние. При аварии между ними load()
            // увидит session_id в журнале и не запишет завершение повторно.
            self.append_event_value("session_completed", deadline_at, event)?;
            self.completed_ids.insert(session_id);
            self.sessions.push(session);
        }
        self.state.status = TimerStatus::Completed;
        self.state.remaining_seconds = 0;
        self.save_state()
    }

    fn remaining_at(&self, now: DateTime<Utc>) -> u64 {
        match self.state.status {
            TimerStatus::Running => self
                .state
                .deadline_at
                .map(|deadline| {
                    let milliseconds = (deadline - now).num_milliseconds().max(0) as u64;
                    milliseconds.div_ceil(1000)
                })
                .unwrap_or(self.state.remaining_seconds),
            TimerStatus::Completed => 0,
            _ => self.state.remaining_seconds,
        }
    }

    fn save_state(&self) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.state)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        atomic_write(&self.data_dir.join("state.json"), &bytes)
    }

    fn append_event(&self, event: &str, now: DateTime<Utc>, data: Value) -> io::Result<()> {
        self.append_event_value(event, now, data)
    }

    fn append_event_value(
        &self,
        event: &str,
        occurred_at: DateTime<Utc>,
        data: Value,
    ) -> io::Result<()> {
        let mut object = json!({
            "v": SCHEMA_VERSION,
            "ts": rfc3339(occurred_at),
            "event": event,
        });
        if let (Some(target), Value::Object(extra)) = (object.as_object_mut(), data) {
            for (key, value) in extra {
                target.insert(key, value);
            }
        }
        let path = self.data_dir.join("events.jsonl");
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(file, "{object}")?;
        file.flush()?;
        file.sync_data()
    }
}

fn local_date(time: DateTime<Utc>) -> NaiveDate {
    time.with_timezone(&Local).date_naive()
}

fn rfc3339(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temp = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_data()?;
    }
    fs::rename(temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "happypath-unit-{}-{}-{}",
            name,
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    }

    fn store(name: &str, duration: u64) -> (PathBuf, StatsStore) {
        let dir = test_dir(name);
        let store = StatsStore::load_from(dir.clone(), duration).unwrap();
        (dir, store)
    }

    fn time(day: u32, hour: u32, second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, day, hour, 0, second)
            .unwrap()
    }

    #[test]
    fn start_pause_resume_and_complete_once() {
        let (dir, mut store) = store("transitions", 20);
        let start = time(2, 9, 0);
        assert_eq!(store.toggle_at(start).unwrap().status, TimerStatus::Running);

        let paused = store.toggle_at(start + Duration::seconds(5)).unwrap();
        assert_eq!(paused.status, TimerStatus::Paused);
        assert_eq!(paused.remaining_seconds, 15);

        let resumed = store.toggle_at(start + Duration::seconds(10)).unwrap();
        assert_eq!(resumed.status, TimerStatus::Running);
        assert_eq!(resumed.remaining_seconds, 15);

        let completed = store.snapshot_at(start + Duration::seconds(25)).unwrap();
        assert_eq!(completed.status, TimerStatus::Completed);
        assert_eq!(completed.total_sessions, 1);
        assert_eq!(completed.total_seconds, 20);
        assert!(completed.today_completed);

        let again = store.snapshot_at(start + Duration::seconds(30)).unwrap();
        assert_eq!(again.total_sessions, 1, "completion must be idempotent");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn running_session_survives_restart_and_completion_is_deduplicated() {
        let (dir, mut store) = store("restart", 1);
        let start = time(2, 9, 0);
        store.toggle_at(start).unwrap();
        store.refresh_at(start + Duration::seconds(2)).unwrap();
        drop(store);

        // Имитируем аварийный дубль последней записи в append-only журнале.
        let events_path = dir.join("events.jsonl");
        let events = fs::read_to_string(&events_path).unwrap();
        let completion = events
            .lines()
            .find(|line| line.contains("session_completed"))
            .unwrap();
        fs::write(&events_path, format!("{events}{completion}\n")).unwrap();

        let mut restored = StatsStore::load_from(dir.clone(), 1).unwrap();
        let snapshot = restored.snapshot_at(start + Duration::seconds(3)).unwrap();
        assert_eq!(snapshot.total_sessions, 1);
        assert_eq!(restored.state.status, TimerStatus::Completed);

        let csv = restored.export_csv().unwrap();
        let content = fs::read_to_string(csv).unwrap();
        assert_eq!(content.lines().count(), 2);
        assert!(content.contains("2026-11-02"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reset_does_not_count_incomplete_session() {
        let (dir, mut store) = store("reset", 20);
        let start = time(2, 9, 0);
        store.toggle_at(start).unwrap();
        let snapshot = store.reset_at(start + Duration::seconds(7)).unwrap();
        assert_eq!(snapshot.status, TimerStatus::Idle);
        assert_eq!(snapshot.total_sessions, 0);
        assert_eq!(snapshot.total_seconds, 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn week_and_all_time_totals_accumulate_across_sessions() {
        let (dir, mut store) = store("week", 1);
        for day in [2, 3] {
            let start = time(day, 9, 0);
            store.toggle_at(start).unwrap();
            store.refresh_at(start + Duration::seconds(2)).unwrap();
        }
        let snapshot = store.snapshot_at(time(4, 9, 0)).unwrap();
        assert_eq!(snapshot.week, [true, true, false, false, false]);
        assert_eq!(snapshot.total_sessions, 2);
        assert_eq!(snapshot.total_seconds, 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn completion_uses_deadline_date_not_late_detection_date() {
        let (dir, mut store) = store("deadline-date", 1);
        let start = time(2, 23, 59);
        store.toggle_at(start).unwrap();
        let snapshot = store.snapshot_at(time(4, 9, 0)).unwrap();
        assert_eq!(snapshot.total_sessions, 1);
        assert_eq!(store.sessions[0].completed_at, start + Duration::seconds(1));
        assert!(!snapshot.today_completed);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn autostart_opt_out_is_persistent_and_reversible() {
        let dir = test_dir("autostart-config");
        assert!(!autostart_disabled(&dir));
        set_autostart_disabled(&dir, true).unwrap();
        assert!(autostart_disabled(&dir));
        set_autostart_disabled(&dir, false).unwrap();
        assert!(!autostart_disabled(&dir));
        fs::remove_dir_all(dir).unwrap();
    }
}
