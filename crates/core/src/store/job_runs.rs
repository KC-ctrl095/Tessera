//! Each run of a scheduled job: when it started and ended, how it went and
//! the chat it ran in (`tessera jobs <id>`, `list_job_runs`). A run's chat
//! also tells which job it belongs to, even after the job was renamed.

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use tessera_proto::{Job, JobRun};

use super::Store;

pub(super) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS job_runs (
    session_id  TEXT PRIMARY KEY,
    job_id      TEXT NOT NULL,
    started_at  INTEGER NOT NULL,
    ended_at    INTEGER,
    outcome     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS job_runs_job ON job_runs(job_id, started_at);
"#;

/// Runs kept per job (the newest).
const KEEP: i64 = 50;

fn run_row(r: &rusqlite::Row) -> rusqlite::Result<JobRun> {
    Ok(JobRun {
        session_id: r.get(0)?,
        job_id: r.get(1)?,
        started_at: r.get::<_, i64>(2)? as u64,
        ended_at: r.get::<_, Option<i64>>(3)?.map(|v| v as u64),
        outcome: r.get(4)?,
    })
}

impl Store {
    /// The admission reservation and run count commit together, or neither
    /// does. CAS refuses an intervening edit/outcome from another store user.
    pub(crate) fn reserve_job_run(
        &self,
        before: &Job,
        job: &Job,
        session: &str,
        at: u64,
    ) -> Result<()> {
        store_call!(self.reserve_job_run(before, job, session, at));
        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction()?;
        let current: String =
            tx.query_row("SELECT data FROM jobs WHERE id = ?1", [&job.id], |r| {
                r.get(0)
            })?;
        if serde_json::to_value(serde_json::from_str::<Job>(&current)?)?
            != serde_json::to_value(before)?
        {
            anyhow::bail!("the task changed during admission; no run was reserved");
        }
        tx.execute(
            "UPDATE jobs SET data = ?2 WHERE id = ?1",
            params![job.id, serde_json::to_string(job)?],
        )?;
        tx.execute("INSERT INTO job_runs(session_id, job_id, started_at, ended_at, outcome) VALUES (?1, ?2, ?3, NULL, 'running')", params![session, job.id, at as i64])?;
        tx.execute("DELETE FROM job_runs WHERE job_id = ?1 AND session_id NOT IN (SELECT session_id FROM job_runs WHERE job_id = ?1 ORDER BY started_at DESC, rowid DESC LIMIT ?2)", params![job.id, KEEP])?;
        tx.commit()?;
        Ok(())
    }

    /// A run of `job` started in `session`.
    pub fn job_run_started(&self, job: &str, session: &str, at: u64) -> Result<()> {
        store_call!(self.job_run_started(job, session, at));
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO job_runs(session_id, job_id, started_at, ended_at, outcome)
             VALUES (?1, ?2, ?3, NULL, 'running')",
            params![session, job, at as i64],
        )?;
        conn.execute(
            "DELETE FROM job_runs WHERE job_id = ?1 AND session_id NOT IN
               (SELECT session_id FROM job_runs WHERE job_id = ?1 ORDER BY started_at DESC LIMIT ?2)",
            params![job, KEEP],
        )?;
        Ok(())
    }

    /// The run in `session` ended (or got going again: `ended_at` None).
    /// Returns the run, and the job when this is still its latest run (its
    /// `last_status` follows; an older run never overwrites a newer one's).
    pub fn job_run_ended(
        &self,
        session: &str,
        outcome: &str,
        ended_at: Option<u64>,
    ) -> Result<Option<(JobRun, Option<Job>)>> {
        store_call!(self.job_run_ended(session, outcome, ended_at));
        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction()?;
        let run = tx
            .query_row(
                "UPDATE job_runs SET outcome = ?2, ended_at = ?3 WHERE session_id = ?1
                 RETURNING session_id, job_id, started_at, ended_at, outcome",
                params![session, outcome, ended_at.map(|v| v as i64)],
                run_row,
            )
            .optional()?;
        let Some(run) = run else {
            return Ok(None);
        };
        let job = tx
            .query_row(
                "UPDATE jobs SET data = json_set(data, '$.last_status', ?2, '$.last_end', ?3)
                 WHERE id = ?4 AND json_extract(data, '$.last_session') = ?1 RETURNING data",
                params![session, outcome, ended_at.map(|v| v as i64), run.job_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        tx.commit()?;
        let job = match job {
            Some(data) => Some(serde_json::from_str::<Job>(&data)?),
            None => None,
        };
        Ok(Some((run, job)))
    }

    /// The run in `session`, if it is one.
    pub fn job_run(&self, session: &str) -> Result<Option<JobRun>> {
        store_call!(self.job_run(session));
        Ok(self
            .conn
            .lock()
            .query_row(
                "SELECT session_id, job_id, started_at, ended_at, outcome FROM job_runs WHERE session_id = ?1",
                [session],
                run_row,
            )
            .optional()?)
    }

    /// `job`'s latest runs, newest first.
    pub fn job_runs(&self, job: &str) -> Result<Vec<JobRun>> {
        store_call!(self.job_runs(job));
        let conn = self.conn.lock();
        let rows = conn
            .prepare(
                "SELECT session_id, job_id, started_at, ended_at, outcome FROM job_runs
                 WHERE job_id = ?1 ORDER BY started_at DESC, rowid DESC",
            )?
            .query_map([job], run_row)?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// A job was deleted: so is the record of its runs (its chats stay).
    pub fn forget_job_runs(&self, job: &str) -> Result<()> {
        store_call!(self.forget_job_runs(job));
        self.conn
            .lock()
            .execute("DELETE FROM job_runs WHERE job_id = ?1", [job])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_are_kept_per_job_and_only_the_latest_sets_the_status() {
        let store = Store::in_memory().unwrap();
        let mut job = Job {
            id: "job_a".into(),
            name: "Check".into(),
            schedule: "@every 1h".into(),
            prompt: "Check".into(),
            enabled: true,
            last_session: Some("s2".into()),
            last_status: Some("running".into()),
            ..Default::default()
        };
        store.put_job(&job).unwrap();
        store.job_run_started("job_a", "s1", 1).unwrap();
        store.job_run_started("job_a", "s2", 2).unwrap();
        // The older run ending says nothing about the job.
        let (run, j) = store
            .job_run_ended("s1", "error", Some(3))
            .unwrap()
            .unwrap();
        assert_eq!((run.outcome.as_str(), run.ended_at), ("error", Some(3)));
        assert!(j.is_none());
        let (_, j) = store.job_run_ended("s2", "ok", Some(4)).unwrap().unwrap();
        let j = j.unwrap();
        assert_eq!(j.last_status.as_deref(), Some("ok"));
        assert_eq!(j.last_end, Some(4));
        job = store.jobs().unwrap().pop().unwrap();
        assert_eq!(job.last_status.as_deref(), Some("ok"));
        assert!(store.job_run_ended("nope", "ok", None).unwrap().is_none());
        let runs = store.job_runs("job_a").unwrap();
        assert_eq!(
            runs.iter()
                .map(|r| r.session_id.as_str())
                .collect::<Vec<_>>(),
            ["s2", "s1"]
        );
        for i in 0..60 {
            store
                .job_run_started("job_a", &format!("x{i}"), 10 + i)
                .unwrap();
        }
        assert_eq!(store.job_runs("job_a").unwrap().len(), KEEP as usize);
        assert_eq!(store.job_run("x59").unwrap().unwrap().outcome, "running");
        store.forget_job_runs("job_a").unwrap();
        assert!(store.job_runs("job_a").unwrap().is_empty());
    }

    #[test]
    fn admission_reservation_count_and_history_commit_atomically_or_not_at_all() {
        let store = Store::in_memory().unwrap();
        let before = Job {
            id: "job_atomic".into(),
            ..Default::default()
        };
        store.put_job(&before).unwrap();
        let admitted = Job {
            runs: 1,
            last_run: Some(1),
            last_session: Some("run_a".into()),
            last_status: Some("running".into()),
            ..before.clone()
        };
        store.conn.lock().execute_batch("CREATE TRIGGER refuse_run BEFORE INSERT ON job_runs BEGIN SELECT RAISE(ABORT, 'fixture write failed'); END;").unwrap();
        assert!(
            store
                .reserve_job_run(&before, &admitted, "run_a", 1)
                .is_err()
        );
        assert_eq!(store.jobs().unwrap()[0].runs, 0);
        assert!(store.job_runs(&before.id).unwrap().is_empty());
        store
            .conn
            .lock()
            .execute_batch("DROP TRIGGER refuse_run;")
            .unwrap();
        store
            .reserve_job_run(&before, &admitted, "run_a", 1)
            .unwrap();
        assert_eq!(store.jobs().unwrap()[0].runs, 1);
        assert!(
            store
                .reserve_job_run(&before, &admitted, "run_b", 1)
                .is_err(),
            "a stale admission cannot overwrite the first"
        );
        assert_eq!(store.job_runs(&before.id).unwrap().len(), 1);
    }
}
