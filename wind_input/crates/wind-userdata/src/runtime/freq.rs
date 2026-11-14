//! 词频与联想历史（对照 `wind-store/src/freq.rs`、`assoc_history.rs`）。两者都是
//! `(code, text) → (count, last_used)`：词频的 code 是码，联想的 code 是上文。

use super::Runtime;
use crate::record::{OpKind, Payload, Target};
use crate::snapshot::SnapshotEntry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FreqRec {
    pub count: u32,
    /// unix 秒
    pub last_used: i64,
}

fn rec(e: &SnapshotEntry) -> FreqRec {
    FreqRec {
        count: e.count,
        last_used: e.last_used,
    }
}

impl Runtime {
    fn incr(&self, target: Target, schema: &str, code: &str, text: &str) -> anyhow::Result<()> {
        if code.is_empty() || text.is_empty() {
            return Ok(());
        }
        self.ud.apply(
            target,
            schema,
            OpKind::Incr,
            code,
            text,
            Payload::Count(1),
            0,
        )?;
        Ok(())
    }

    fn clear_target(&self, target: Target, schema: &str) -> anyhow::Result<usize> {
        self.ud.clear(target, schema)
    }

    /// 同键取 max（count 与 last_used 各自取）；新键 created_at = last_used。
    fn import_max(
        &self,
        target: Target,
        schema: &str,
        rows: &[(String, String, FreqRec)],
    ) -> anyhow::Result<usize> {
        self.ud.fold_with(target, schema, &mut |v| {
            let mut map: std::collections::BTreeMap<(String, String), SnapshotEntry> = v
                .into_iter()
                .map(|e| ((e.code.clone(), e.text.clone()), e))
                .collect();
            for (code, text, r) in rows {
                if code.is_empty() || text.is_empty() {
                    continue;
                }
                let e = map
                    .entry((code.clone(), text.clone()))
                    .or_insert_with(|| SnapshotEntry {
                        code: code.clone(),
                        text: text.clone(),
                        weight: 0,
                        boundary: 0,
                        count: 0,
                        created_at: r.last_used,
                        last_used: 0,
                        flags: 0,
                        order: 0,
                    });
                e.count = e.count.max(r.count);
                e.last_used = e.last_used.max(r.last_used);
            }
            map.into_values().collect()
        })?;
        Ok(rows.len())
    }

    pub fn record_freq(&self, schema: &str, code: &str, text: &str) -> anyhow::Result<()> {
        self.incr(Target::Freq, schema, code, text)
    }

    pub fn freq(&self, schema: &str, code: &str, text: &str) -> Option<FreqRec> {
        self.ud
            .get(Target::Freq, schema, code, text)
            .map(|e| rec(&e))
    }

    pub fn freq_batch(&self, keys: &[(String, String, String)]) -> Vec<Option<FreqRec>> {
        keys.iter().map(|(s, c, t)| self.freq(s, c, t)).collect()
    }

    pub fn for_each_freq(
        &self,
        schema: &str,
        prefix: &str,
        f: &mut dyn FnMut(&str, &str, FreqRec) -> bool,
    ) {
        self.ud.scan(Target::Freq, schema, prefix, &mut |e| {
            f(&e.code, &e.text, rec(&e))
        });
    }

    pub fn list_freq_paged(
        &self,
        schema: &str,
        prefix: &str,
        offset: usize,
        limit: usize,
    ) -> (Vec<(String, String, FreqRec)>, usize) {
        let mut page = Vec::new();
        let mut total = 0;
        self.for_each_freq(schema, prefix, &mut |c, t, r| {
            if total >= offset && (limit == 0 || page.len() < limit) {
                page.push((c.to_string(), t.to_string(), r));
            }
            total += 1;
            true
        });
        (page, total)
    }

    pub fn delete_freq(&self, schema: &str, code: &str, text: &str) -> anyhow::Result<()> {
        if self.ud.get(Target::Freq, schema, code, text).is_some() {
            self.ud.apply(
                Target::Freq,
                schema,
                OpKind::Delete,
                code,
                text,
                Payload::None,
                0,
            )?;
        }
        Ok(())
    }

    pub fn clear_freq(&self, schema: &str) -> anyhow::Result<usize> {
        self.clear_target(Target::Freq, schema)
    }

    pub fn import_freq_rows(
        &self,
        schema: &str,
        rows: &[(String, String, FreqRec)],
    ) -> anyhow::Result<usize> {
        self.import_max(Target::Freq, schema, rows)
    }

    pub fn record_assoc_pick(&self, schema: &str, context: &str, word: &str) -> anyhow::Result<()> {
        if schema.is_empty() {
            return Ok(());
        }
        self.incr(Target::Assoc, schema, context, word)
    }

    pub fn assoc_history(
        &self,
        schema: &str,
        context: &str,
        limit: usize,
    ) -> Vec<(String, FreqRec)> {
        let mut out: Vec<(String, FreqRec)> = self
            .ud
            .exact(Target::Assoc, schema, context)
            .into_iter()
            .map(|e| {
                let r = rec(&e);
                (e.text, r)
            })
            .collect();
        out.sort_by(|a, b| {
            (b.1.count, b.1.last_used)
                .cmp(&(a.1.count, a.1.last_used))
                .then_with(|| a.0.cmp(&b.0))
        });
        if limit > 0 {
            out.truncate(limit);
        }
        out
    }

    pub fn for_each_assoc(&self, schema: &str, f: &mut dyn FnMut(&str, &str, FreqRec) -> bool) {
        self.ud.scan(Target::Assoc, schema, "", &mut |e| {
            f(&e.code, &e.text, rec(&e))
        });
    }

    pub fn delete_assoc(&self, schema: &str, context: &str, word: &str) -> anyhow::Result<()> {
        if self.ud.get(Target::Assoc, schema, context, word).is_some() {
            self.ud.apply(
                Target::Assoc,
                schema,
                OpKind::Delete,
                context,
                word,
                Payload::None,
                0,
            )?;
        }
        Ok(())
    }

    pub fn clear_assoc(&self, schema: &str) -> anyhow::Result<usize> {
        self.clear_target(Target::Assoc, schema)
    }

    pub fn import_assoc_rows(
        &self,
        schema: &str,
        rows: &[(String, String, FreqRec)],
    ) -> anyhow::Result<usize> {
        self.import_max(Target::Assoc, schema, rows)
    }
}
