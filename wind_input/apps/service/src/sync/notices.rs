//! 立即同步与结果提示：主动同步的票、完成 / 失败 / 忙 / 未启用提示，以及后台失败与冲突只提示一次。

use super::*;

/// 同步结果提示（由接线处弹 toast）。
#[derive(Debug, Clone, PartialEq)]
pub enum SyncNotice {
    Done(RoundStats),
    Failed(String),
    Busy,
    Disabled,
    BackgroundFailed(String),
    /// 后台轮产生冲突（K 个）。
    BackgroundConflict(u32),
}

pub fn notice_text(n: &SyncNotice) -> String {
    match n {
        SyncNotice::Done(s) => {
            let mut parts = Vec::new();
            if s.downloaded > 0 {
                parts.push(format!("收到 {} 个文件", s.downloaded));
            }
            if s.uploaded > 0 {
                parts.push(format!("上传 {} 个", s.uploaded));
            }
            let mut t = "同步完成".to_string();
            if !parts.is_empty() {
                t.push('：');
                t.push_str(&parts.join("、"));
            }
            if s.conflicts > 0 {
                t.push_str(&format!("，{} 个冲突", s.conflicts));
            }
            t
        }
        SyncNotice::Failed(r) => format!("同步失败：{r}"),
        SyncNotice::Busy => "正在同步，完成后会提示".into(),
        SyncNotice::Disabled => "数据同步未启用".into(),
        SyncNotice::BackgroundFailed(r) => format!("数据同步出错：{r}"),
        SyncNotice::BackgroundConflict(k) => format!("同步产生 {k} 个冲突，请到设置页处理"),
    }
}

/// `now_in_flight` 的 RAII 守卫：drop 时清标志。
struct InFlight<'a>(&'a AtomicBool);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl SyncHost {
    pub(super) fn evaluate(&self, inner: &mut Inner) -> Vec<SyncNotice> {
        let mut out = Vec::new();
        let (phase, last_error) = self.phase_of(inner);
        let linked = inner.link.is_some();
        let mut resolved = false;
        // 同步进行中不计入票的等待时长：超时从最近一次非同步中起算
        if phase == "syncing" {
            let now = self.now();
            match &mut inner.track.ticket {
                Some(Active::Ticket { since, .. }) | Some(Active::Pending { since }) => {
                    *since = now
                }
                None => {}
            }
        }
        // 一连上就带着旧错误：当作「已在失败中」，不为它弹
        if linked && !inner.track.baseline_seen && !inner.status.phase.is_empty() {
            inner.track.baseline_seen = true;
            if inner.status.round == 0 && phase == "error" && last_error.is_some() {
                inner.track.was_error = true;
            }
        }
        match &inner.track.ticket {
            Some(Active::Ticket { ticket, since }) => {
                if linked && inner.status.round >= *ticket {
                    out.push(match &inner.status.last_round {
                        Some(r) if !r.ok => SyncNotice::Failed(
                            r.error.clone().unwrap_or_else(|| "未知错误".to_string()),
                        ),
                        Some(r) => SyncNotice::Done(r.clone()),
                        None => SyncNotice::Done(RoundStats {
                            ok: true,
                            ..Default::default()
                        }),
                    });
                    resolved = true;
                    inner.track.ticket = None;
                } else if let Some(why) = self.abandon_reason(inner, &phase, &last_error, *since) {
                    out.push(SyncNotice::Failed(why));
                    inner.track.ticket = None;
                } else if !linked {
                    out.push(SyncNotice::Failed("同步进程已退出".into()));
                    inner.track.ticket = None;
                }
            }
            Some(Active::Pending { since }) => {
                let why = self
                    .abandon_reason(inner, &phase, &last_error, *since)
                    .or_else(|| {
                        (!linked && phase == "error").then(|| {
                            last_error
                                .clone()
                                .unwrap_or_else(|| "同步进程频繁退出，已停止重启".into())
                        })
                    });
                if let Some(why) = why {
                    out.push(SyncNotice::Failed(why));
                    inner.track.ticket = None;
                }
            }
            None => {}
        }
        // 后台失败只看轮次：新一轮失败且无主动同步在管才提示；成功一轮复位。
        let round = inner.status.round;
        if linked && round > inner.track.last_seen_round {
            inner.track.last_seen_round = round;
            if let Some(r) = &inner.status.last_round {
                if r.ok {
                    inner.track.was_error = false;
                    if r.conflicts == 0 {
                        inner.track.conflict_notified = false;
                    } else {
                        // 主动轮的冲突已写进完成提示，只置位不另弹
                        if !resolved && !inner.track.conflict_notified {
                            out.push(SyncNotice::BackgroundConflict(r.conflicts));
                        }
                        inner.track.conflict_notified = true;
                    }
                } else {
                    if !inner.track.was_error && !resolved && inner.track.ticket.is_none() {
                        out.push(SyncNotice::BackgroundFailed(
                            r.error
                                .clone()
                                .or(last_error)
                                .unwrap_or_else(|| "未知错误".into()),
                        ));
                    }
                    inner.track.was_error = true;
                }
            }
        }
        // 同步进程反复退出被熔断：进入熔断提示一次，复位仍等之后成功的一轮。
        let tripped = inner.link.is_none() && phase == "error" && inner.sup.is_tripped();
        if tripped && !inner.track.was_tripped {
            if !inner.track.was_error {
                out.push(SyncNotice::BackgroundFailed(
                    "同步进程反复退出，已停止自动同步".into(),
                ));
            }
            inner.track.was_error = true;
        }
        inner.track.was_tripped = tripped;
        out
    }

    /// 等票期间同步已不可能出结果时返回放弃原因：停用 / 未安装 / 停摆 / 待确认身份 / 超时。
    pub(super) fn abandon_reason(
        &self,
        inner: &Inner,
        phase: &str,
        last_error: &Option<String>,
        since: Instant,
    ) -> Option<String> {
        let default = match phase {
            "disabled" => "已停用",
            "not_installed" => "同步组件未安装",
            "halted" => "同步已停止",
            _ if inner.status.needs_identity => "需要先确认本机身份",
            _ if self.now().saturating_duration_since(since) >= TICKET_TIMEOUT => {
                // 超时原因固定，不借用可能过时的 last_error
                return Some("等待超过 10 分钟".into());
            }
            _ => return None,
        };
        Some(last_error.clone().unwrap_or_else(|| default.to_string()))
    }

    pub(super) fn notify_now(&self, n: SyncNotice) {
        if let Some(f) = self.notify.get() {
            f(n);
        }
    }

    /// 起 `sync-now` 工作线程发 `SyncNow`；已有一个在发就不再起。
    pub(super) fn spawn_sync_now(&self) {
        let Some(me) = self.me.upgrade() else { return };
        if me.now_in_flight.swap(true, Ordering::AcqRel) {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("sync-now".into())
            .spawn({
                let me = me.clone();
                move || {
                    // 守卫保证 panic 时也会清标志；正常路径先清再提示，
                    // 这样提示回调里再点「立即同步」不会被旧标志吞掉。
                    let guard = InFlight(&me.now_in_flight);
                    let notices = me.send_sync_now();
                    drop(guard);
                    for n in notices {
                        me.notify_now(n);
                    }
                    me.pump();
                    // 清标志前后若有人又登记了待发请求，补一个线程。
                    let wants =
                        matches!(lock(&me.inner).track.ticket, Some(Active::Pending { .. }))
                            && lock(&me.inner).link.is_some();
                    if wants {
                        me.spawn_sync_now();
                    }
                }
            })
            .is_ok();
        if !spawned {
            me.now_in_flight.store(false, Ordering::Release);
        }
    }

    /// 发 `SyncNow` 并把回执换成票号，返回要弹的提示。只有回执回来时那张待发请求还在才登记、提示：
    /// 期间已被放弃（停用、超时）的不再报。
    pub(super) fn send_sync_now(&self) -> Vec<SyncNotice> {
        if !matches!(lock(&self.inner).track.ticket, Some(Active::Pending { .. })) {
            return Vec::new();
        }
        let since = self.now();
        let reply = self.command(Op::SyncNow, Value::Null);
        let mut notices = Vec::new();
        let mut inner = lock(&self.inner);
        if !matches!(inner.track.ticket, Some(Active::Pending { .. })) {
            return notices;
        }
        match reply {
            Ok(v) => match v.get("ticket").and_then(Value::as_u64) {
                Some(ticket) => {
                    if v.get("busy").and_then(Value::as_bool).unwrap_or(false) {
                        notices.push(SyncNotice::Busy);
                    }
                    inner.track.ticket = Some(Active::Ticket { ticket, since });
                }
                None => {
                    inner.track.ticket = None;
                    notices.push(SyncNotice::Failed("同步进程回执缺少票号".into()));
                }
            },
            Err(e) => {
                inner.track.ticket = None;
                notices.push(SyncNotice::Failed(e));
            }
        }
        notices
    }

    /// 用户主动「立即同步」：只做快速检查就返回，结果经提示送达。
    /// 未启用不拉起进程；进程还在启动则连上后补发。
    pub fn sync_now_active(&self) -> Value {
        let linked = {
            let mut inner = lock(&self.inner);
            if !inner.settings.enabled {
                drop(inner);
                self.notify_now(SyncNotice::Disabled);
                return json!({ "outcome": "disabled" });
            }
            if !(self.installed)() {
                drop(inner);
                let e = "同步组件未安装".to_string();
                self.notify_now(SyncNotice::Failed(e.clone()));
                return json!({ "outcome": "failed", "error": e });
            }
            // 已有等待中的票时再点不发新请求，弹出的是那张票的结果。
            let had_ticket = inner.track.ticket.is_some();
            if !had_ticket {
                inner.track.ticket = Some(Active::Pending { since: self.now() });
            }
            let linked = inner.link.is_some();
            if had_ticket {
                drop(inner);
                self.notify_now(SyncNotice::Busy);
                return json!({ "outcome": "requested" });
            }
            linked
        };
        if linked {
            self.spawn_sync_now();
        }
        json!({ "outcome": "requested" })
    }
}
