//! 设备组与身份：确认身份、改号、组名、加入 / 退出组、认领设备。

use super::*;

impl Runner {
    /// 身份操作的前提：已持有密钥与存储凭据。
    pub(super) fn identity_ctx(&self) -> Result<(Dek, Box<dyn Backend>), String> {
        let dek = self.dek().map_err(|_| "本机尚未加入加密同步".to_string())?;
        Ok((dek, self.fresh_backend(None)?))
    }

    /// 身份变更之后：存状态、重写名册、解除停止态、稍后同步。
    pub(super) fn after_identity_change(&mut self) {
        self.info_written = None;
        self.after_user_fix();
        self.publish_info(false);
    }

    /// 覆盖本机设置用的本地存储：与 `join_group` 一致。
    pub(super) fn local_all(&self) -> FsLocal {
        FsLocal::new(
            self.cfg.userdata_dir.clone(),
            self.cfg.link.clone(),
            self.echo.clone(),
        )
    }

    /// 用组的设置覆盖本机前备份本机设置。
    pub(super) fn backup_local(&self, local: &FsLocal) -> Result<std::path::PathBuf, String> {
        let paths = local.list().map_err(|e| e.0)?;
        let dir = backup::backup_settings(&self.cfg.userdata_dir, &paths, now_unix())?;
        log!("已备份本机设置");
        Ok(dir)
    }

    /// 本机设置文件的（路径, 内容摘要），判断覆盖是否已经动过本机。
    pub(super) fn settings_digest(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .local_all()
            .list()
            .unwrap_or_default()
            .into_iter()
            .filter(|p| kind(p) == Kind::Setting)
            .map(|p| {
                let h = std::fs::read(self.abs(&p))
                    .map(|b| content_hash(&b))
                    .unwrap_or_default();
                (p, h)
            })
            .collect();
        v.sort();
        v
    }

    /// 覆盖本机设置失败：本机已被改动一部分则进停止态，免得半成品被当本机修改同步进旧组。
    pub(super) fn join_failed(&mut self, before: &[(String, String)], msg: String) -> String {
        if self.settings_digest() == before {
            return msg;
        }
        log!("加入设备组中途失败，本机设置已部分改写: {msg}");
        self.state.retry.halted = Some(JOIN_INCOMPLETE.into());
        self.state.retry.last_error = Some(JOIN_INCOMPLETE.into());
        self.save_state();
        format!("{JOIN_INCOMPLETE}（{msg}）")
    }

    /// 号已被占用：名册里有，或学习通道下有数据（已移除设备的贡献仍在计入，占它的号会把两份当成一台）。
    pub(super) fn id_used(b: &dyn Backend, id: &str) -> Result<bool, String> {
        let se = |e: SyncError| sync_error(&e);
        Ok(id_taken(b, id).map_err(se)?
            || wind_sync::learn::lane::learn_devices(b)
                .map_err(se)?
                .contains(id))
    }

    /// 本机号已被远端他人占用时改用首个空闲的 `<号>-<n>`。只用于未确认身份时：本机从未以这个号发布过。
    pub(super) fn ensure_free_id(&mut self, b: &dyn Backend) -> Result<(), String> {
        let se = |e: SyncError| sync_error(&e);
        let base = self.state.device.clone();
        if !Self::id_used(b, &base)? {
            return Ok(());
        }
        for n in 2..1000 {
            let id = ids::with_suffix(&base, n);
            if !Self::id_used(b, &id)? {
                identity::rename_device(b, &mut self.state, &id).map_err(se)?;
                // 旧号属于别的设备，不能当本机旧号删除
                self.state.retired_ids.retain(|x| x.id != base);
                log!("设备号 {base} 已被占用，本机改用 {id}");
                return Ok(());
            }
        }
        Err(format!("找不到空闲的设备号（{base}-n）"))
    }

    /// 以当前设备号新建单机组并确认身份。
    pub(super) fn cmd_confirm_identity(&mut self) -> Result<Value, String> {
        // 已确认则不动（不把已在组里的设备拉回单机组）
        if self.state.identity_confirmed {
            let (device, group) = (self.state.device.clone(), group_of(&self.state));
            return Ok(json!({ "ok": true, "device": device, "group": group }));
        }
        let (dek, b) = self.identity_ctx()?;
        self.ensure_free_id(&*b)?;
        let device = self.state.device.clone();
        let roster = list_devices(&*b, &dek.devinfo_key()).map_err(|e| sync_error(&e))?;
        let group = free_group(&roster, &device, &[&device], None)?;
        identity::leave_group(&mut self.state, &group);
        self.state.group_name.clear();
        self.state.group_name_at = 0;
        self.state.identity_confirmed = true;
        self.after_identity_change();
        log!("已确认本机身份（单机组 {group}）");
        Ok(json!({ "ok": true, "device": device, "group": group }))
    }

    pub(super) fn cmd_rename_device(&mut self, args: &Value) -> Result<Value, String> {
        let id = str_arg(args, "id")
            .filter(|s| !s.is_empty())
            .ok_or("缺少参数 id")?;
        if id == self.state.device {
            return Err("新设备号与当前相同".into());
        }
        let (_, b) = self.identity_ctx()?;
        if Self::id_used(&*b, id)? {
            return Err(format!("设备号 {id} 不可用"));
        }
        let old = self.state.device.clone();
        // 撞号时旧号另有主人，未确认身份时本机从未以旧号发布过：这两种情况旧号都不能删
        let retire = self.state.identity_confirmed && !self.id_conflict();
        if self.state.group.is_empty() {
            self.state.group = old.clone();
        }
        identity::rename_device(&*b, &mut self.state, id).map_err(|e| sync_error(&e))?;
        if !retire {
            self.state.retired_ids.retain(|x| x.id != old);
        }
        if let Ok(dek) = self.dek() {
            match self.with_learn(|c| learn_round::rename_own(c, &*b, &dek.files_key(), retire)) {
                Some(Ok(ws)) => ws.iter().for_each(|w| log!("学习数据: {w}")),
                Some(Err(e)) => log!("学习数据改号未完成，下一轮重试: {e}"),
                None => {}
            }
        }
        self.after_identity_change();
        log!("设备号已改为 {id}");
        Ok(json!({ "ok": true, "device": id }))
    }

    pub(super) fn cmd_set_group_name(&mut self, args: &Value) -> Result<Value, String> {
        let name = str_arg(args, "name").ok_or("缺少参数 name")?.trim();
        if name.chars().count() > wind_sync_ipc::MAX_DEVICE_NAME_CHARS {
            return Err(format!(
                "组名不能超过 {} 个字符",
                wind_sync_ipc::MAX_DEVICE_NAME_CHARS
            ));
        }
        if self.state.group.is_empty() {
            self.state.group = self.state.device.clone();
        }
        self.state.group_name = name.to_string();
        // 时钟回拨时也要比本机旧值新，才能在名册里胜出
        self.state.group_name_at = now_unix().max(self.state.group_name_at + 1);
        self.save_state();
        self.publish_info(false);
        Ok(json!({ "ok": true }))
    }

    pub(super) fn cmd_join_group(&mut self, args: &Value) -> Result<Value, String> {
        let group = str_arg(args, "group").unwrap_or_default();
        if !ids::valid_id(group) {
            return Err(format!("组号 {group} 不可用"));
        }
        let mode = match str_arg(args, "mode") {
            Some("theirs") => Overwrite::Theirs,
            Some("mine") => Overwrite::Mine,
            _ => return Err("参数 mode 须为 theirs 或 mine".into()),
        };
        let (dek, b) = self.identity_ctx()?;
        let roster = list_devices(&*b, &dek.devinfo_key()).map_err(|e| sync_error(&e))?;
        let class = self.cfg.class;
        // 未确认身份时本机从未发布过，名册里同号的条目是别人的
        let me = self
            .state
            .identity_confirmed
            .then_some(self.state.device.as_str());
        let exists = roster.iter().any(|(d, r)| {
            Some(d.as_str()) != me
                && r.as_ref()
                    .is_ok_and(|i| i.group == group && class_of_platform(&i.platform) == class)
        });
        if !exists {
            return Err(format!(
                "设备组 {group} 不存在，或其成员与本机不是同一类设备（桌面与手机不能同组）"
            ));
        }
        if !self.state.identity_confirmed {
            self.ensure_free_id(&*b)?;
        }
        let mut local = self.local_all();
        if mode == Overwrite::Theirs {
            self.backup_local(&local)?;
        }
        let before = self.settings_digest();
        let ns = Namespace::new(self.cfg.class, group);
        let mut state = self.state.clone();
        let rep = match identity::join_group(&*b, &mut local, &mut state, &dek, &ns, mode) {
            Ok(r) => r,
            Err(e) => return Err(self.join_failed(&before, sync_error(&e))),
        };
        state.group_name.clear();
        state.group_name_at = 0;
        adopt_group_name(&mut state, &roster);
        state.identity_confirmed = true;
        clear_join_halt(&mut state);
        self.state = state;
        self.after_identity_change();
        log!(
            "已加入设备组 {group}：写入 {}，删除 {}",
            rep.written.len(),
            rep.deleted.len()
        );
        Ok(json!({
            "ok": true, "group": group,
            "written": rep.written.len(), "deleted": rep.deleted.len(),
        }))
    }

    /// 以本机设置新建单机组；组号取设备号，已被占用（含当前组）则加序号。
    pub(super) fn cmd_leave_group(&mut self, args: &Value) -> Result<Value, String> {
        if !self.state.identity_confirmed {
            return Err(IDENTITY_MSG.into());
        }
        let name = str_arg(args, "name").map(str::trim).unwrap_or_default();
        if name.chars().count() > wind_sync_ipc::MAX_DEVICE_NAME_CHARS {
            return Err(format!(
                "组名不能超过 {} 个字符",
                wind_sync_ipc::MAX_DEVICE_NAME_CHARS
            ));
        }
        let (dek, b) = self.identity_ctx()?;
        let roster = list_devices(&*b, &dek.devinfo_key()).map_err(|e| sync_error(&e))?;
        let device = self.state.device.clone();
        let current = group_of(&self.state).to_string();
        let group = free_group(&roster, &device, &[&device], Some(&current))?;
        identity::leave_group(&mut self.state, &group);
        self.state.group_name = name.to_string();
        self.state.group_name_at = if name.is_empty() { 0 } else { now_unix() };
        self.after_identity_change();
        log!("已退出设备组 {current}，新建单机组 {group}");
        Ok(json!({ "ok": true, "group": group }))
    }

    /// 认领已有设备：沿用它的号与组，用组的设置覆盖本机（先备份）。
    pub(super) fn cmd_claim_device(&mut self, args: &Value) -> Result<Value, String> {
        let dev = str_arg(args, "dev")
            .filter(|d| !d.is_empty())
            .ok_or("缺少参数 dev")?;
        if self.state.identity_confirmed && dev == self.state.device {
            return Err("不能认领本机".into());
        }
        let (dek, b) = self.identity_ctx()?;
        let se = |e: SyncError| sync_error(&e);
        let roster = list_devices(&*b, &dek.devinfo_key()).map_err(se)?;
        // 跨类别认领会把对方那一类的设置组带过来（手机拿到电脑的设置），界面已过滤，这里兜底
        let class = self.cfg.class;
        if roster.iter().any(|(d, r)| {
            d == dev
                && r.as_ref()
                    .is_ok_and(|i| class_of_platform(&i.platform) != class)
        }) {
            return Err(format!(
                "{dev} 与本机不是同一类设备（桌面与手机），不能恢复为它"
            ));
        }
        // 本机原号在远端已有学习通道：认领后它作为他机继续计入，本机现有数据与统计都已在那里
        let old_counted = self.state.identity_confirmed
            && !self.id_conflict()
            && wind_sync::learn::lane::learn_devices(&*b)
                .map_err(se)?
                .contains(&self.state.device);
        let mut local = self.local_all();
        self.backup_local(&local)?;
        let mut state = self.state.clone();
        let mut group = identity::claim_device(&*b, &mut state, &dek, dev).map_err(se)?;
        let from_roster = roster
            .iter()
            .any(|(d, r)| d == dev && r.as_ref().is_ok_and(|i| i.group == group));
        if from_roster {
            let before = self.settings_digest();
            let ns = Namespace::new(self.cfg.class, group.clone());
            if let Err(e) =
                identity::join_group(&*b, &mut local, &mut state, &dek, &ns, Overwrite::Theirs)
            {
                return Err(self.join_failed(&before, sync_error(&e)));
            }
        } else {
            // 名册没给出组号（缺失或不合法）：回退的组号与他人查重；组未知就不覆盖本机设置
            let fallback = free_group(&roster, dev, &[dev], None)?;
            if fallback == group {
                let before = self.settings_digest();
                let ns = Namespace::new(self.cfg.class, group.clone());
                if let Err(e) =
                    identity::join_group(&*b, &mut local, &mut state, &dek, &ns, Overwrite::Theirs)
                {
                    return Err(self.join_failed(&before, sync_error(&e)));
                }
            } else {
                identity::leave_group(&mut state, &fallback);
                group = fallback;
            }
        }
        state.identity_confirmed = true;
        clear_join_halt(&mut state);
        // 已确认过身份的设备改认领别的号：原号与待删旧号都归入待删（撞号时原号另有主人，不删）
        if self.state.identity_confirmed && !self.id_conflict() {
            state.retired_ids = std::mem::take(&mut self.state.retired_ids);
            state.retired_ids.push(wind_sync::state::RetiredId {
                id: self.state.device.clone(),
                nonce: self.state.last_nonce.clone(),
                gen_: self.state.gen_,
            });
        }
        self.state = state;
        // 原号作为他机继续计入（其学习数据保留），本机贡献换成认领号的；初始化失败时状态已删，
        // 下一轮 ensure_state 按「远端有本机贡献」重新认领
        let how = learn_round::Joined::Claim {
            dev: dev.into(),
            old_counted,
        };
        let learned = self.learn_join(how, &*b);
        self.reload_learn_clock();
        self.after_identity_change();
        log!("已认领设备 {dev}（组 {group}）");
        learned.map_err(|e| format!("已认领设备 {dev}，但{e}（下一轮重试）"))?;
        Ok(json!({ "ok": true, "device": dev, "group": group }))
    }
}
