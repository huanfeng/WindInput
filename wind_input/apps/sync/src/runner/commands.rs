//! 密钥与存储相关的命令：凭据、测试连接、同步密钥（生成 / 导入 / 查看）、立即同步、设备列表、重置远端。

use super::*;

impl Runner {
    /// 用户处理完问题（改凭据 / 同步密钥）后：解除停止态、丢旧连接、稍后同步。
    pub(super) fn after_user_fix(&mut self) {
        self.clear_halt();
        self.save_state();
        self.kick();
    }

    pub(super) fn cmd_set_secret(&mut self, args: &Value) -> Result<Value, String> {
        match args.get("password") {
            Some(Value::String(p)) => {
                self.secrets.password = Some(p.clone());
                self.secrets.backend_id = backend_fingerprint(self.settings.backend.as_ref());
            }
            Some(Value::Null) => {
                self.secrets.password = None;
                self.secrets.backend_id = None;
            }
            _ => return Err("缺少参数 password（字符串；null 表示清除）".into()),
        }
        self.save_secrets()?;
        self.after_user_fix();
        Ok(json!({ "ok": true }))
    }

    /// 不经缓存、可带临时密码地建后端：测试连接与同步密钥命令都用它。
    pub(super) fn fresh_backend(&self, password: Option<&str>) -> Result<Box<dyn Backend>, String> {
        if self.settings.backend.is_none() {
            return Err("尚未配置存储后端".into());
        }
        let pw = password
            .map(str::to_string)
            .or_else(|| self.secrets.password.clone())
            .ok_or("尚未保存存储凭据（密码或访问密钥）")?;
        self.factory.build(&self.settings, &pw)
    }

    pub(super) fn read_meta(b: &dyn Backend) -> Result<Option<Meta>, String> {
        match b
            .get(&manifest::meta_key())
            .map_err(|e| backend_error(&e))?
        {
            None => Ok(None),
            Some(raw) => Meta::from_json(&raw)
                .map(Some)
                .map_err(|e| crypto_error(&e)),
        }
    }

    pub(super) fn cmd_test_connection(&mut self, args: &Value) -> Result<Value, String> {
        let b = self.fresh_backend(str_arg(args, "password"))?;
        b.test_connection().map_err(|e| backend_error(&e))?;
        let meta = Self::read_meta(&*b)?;
        let joined = match (&meta, self.dek()) {
            (Some(m), Ok(d)) => m.verify(&d),
            _ => false,
        };
        if joined {
            self.mark_verified(library_id(self.settings.backend.as_ref()));
        } else {
            self.invalidate_verified();
        }
        Ok(json!({ "ok": true, "remote_initialized": meta.is_some(), "joined": joined }))
    }

    /// 本机先存下密钥（发布远端 meta 之前），返回旧值供回滚。保存失败则不改动。
    pub(super) fn store_dek(&mut self, dek: &Dek) -> Result<Option<String>, String> {
        let prev = self.secrets.dek.replace(dek.to_base64());
        if let Err(e) = self.save_secrets() {
            self.secrets.dek = prev;
            return Err(e);
        }
        Ok(prev)
    }

    /// 远端发布失败：把本机密钥退回原值。
    pub(super) fn rollback_dek(&mut self, prev: Option<String>) {
        self.secrets.dek = prev;
        if let Err(e) = self.save_secrets() {
            log!("回滚本机同步密钥失败: {e}");
        }
    }

    /// DEK 变了说明换了同步库：旧的清单基线对新远端没有意义，重建（沿用设备号，回到单机组）。
    /// 密钥变了：只有初始化（`init`）直接确认身份，其余一律按加入处理、待确认；密钥没变则保持原有身份。
    pub(super) fn finish_adopt(&mut self, changed: bool, init: bool) {
        if changed {
            let device = self.state.device.clone();
            self.state = SyncState::new(device.clone());
            self.state.group = device;
            self.state.identity_confirmed = init;
            self.info_written = None;
        }
        self.mark_verified(library_id(self.settings.backend.as_ref()));
        self.after_user_fix();
        self.publish_info(false);
    }

    pub(super) fn adopt_dek(&mut self, dek: &Dek, init: bool) -> Result<(), String> {
        let prev = self.store_dek(dek)?;
        let changed = prev.as_deref() != Some(dek.to_base64().as_str());
        self.finish_adopt(changed, init);
        Ok(())
    }

    /// 远端还没有同步库：生成同步密钥并建库，本机身份自动确认；远端已有则拒绝。
    pub(super) fn cmd_create_key(&mut self) -> Result<Value, String> {
        let b = self.fresh_backend(None)?;
        if Self::read_meta(&*b)?.is_some() {
            return Err(sync_error(&SyncError::AlreadyInitialized));
        }
        let dek = Dek::generate();
        // 先存本机再发布：本机存不下就不建库，免得远端有库而用户没拿到密钥
        let prev = self.store_dek(&dek)?;
        let published = remote::init_remote_with(&*b, &dek).map_err(|e| sync_error(&e));
        // 已知局限：回读与此处复核之间仍有极小的并发创建窗口
        let verified = published.and_then(|_| match Self::read_meta(&*b)? {
            Some(m) if m.verify(&dek) => Ok(()),
            _ => Err(sync_error(&SyncError::AlreadyInitialized)),
        });
        if let Err(e) = verified {
            self.rollback_dek(prev);
            return Err(e);
        }
        let changed = prev.as_deref() != Some(dek.to_base64().as_str());
        self.finish_adopt(changed, true);
        log!("已生成同步密钥");
        Ok(json!({ "ok": true, "key": dek.to_key_string() }))
    }

    /// 粘贴同步密钥加入已有同步库；身份待确认。
    pub(super) fn cmd_import_key(&mut self, args: &Value) -> Result<Value, String> {
        let key = str_arg(args, "key").unwrap_or_default();
        let b = self.fresh_backend(None)?;
        if Self::read_meta(&*b)?.is_none() {
            return Err(sync_error(&SyncError::NotInitialized));
        }
        let dek = remote::join_with_key(&*b, key).map_err(|e| sync_error(&e))?;
        self.adopt_dek(&dek, false)?;
        Ok(json!({ "ok": true }))
    }

    pub(super) fn cmd_export_key(&self) -> Result<Value, String> {
        let dek = self
            .dek()
            .map_err(|_| "本机尚未加入加密同步，没有可导出的同步密钥".to_string())?;
        Ok(json!({ "key": dek.to_key_string() }))
    }

    pub(super) fn cmd_sync_now(&mut self) -> Result<Value, String> {
        if !self.configured() {
            return Err("同步未启用或尚未配置存储后端".into());
        }
        if self.secrets.password.is_none() {
            return Err("尚未保存存储凭据".into());
        }
        if self.secrets.dek.is_none() {
            return Err("尚未生成或粘贴同步密钥".into());
        }
        if !self.state.identity_confirmed {
            return Err(IDENTITY_MSG.into());
        }
        if self.join_incomplete() {
            return Err(JOIN_INCOMPLETE.into());
        }
        let ticket = self.round_no + 1;
        if self.syncing {
            // 进行中的这一轮结束即算完成，不再另排一轮
            self.clear_halt_after_round = true;
            return Ok(json!({ "queued": false, "busy": true, "ticket": ticket }));
        }
        self.clear_halt();
        self.force_now = true;
        Ok(json!({ "queued": true, "ticket": ticket }))
    }

    /// 大批量删除护栏停止后，用户确认「这些删除是真的」：解除停止态，下一轮放行一次。
    pub(super) fn cmd_confirm_mass_delete(&mut self) -> Result<Value, String> {
        let pending = self.state.retry.halted.as_deref().is_some_and(|h| {
            h.contains(MASS_MARK) || h.starts_with(wind_sync::error::MASS_DELETION_PREFIX)
        });
        if !pending {
            return Err("当前没有等待确认的大批量删除".into());
        }
        self.state.retry.clear_halt();
        self.allow_mass_once = true;
        self.force_now = true;
        self.save_state();
        Ok(json!({ "queued": true }))
    }

    pub(super) fn cmd_list_devices(&mut self) -> Result<Value, String> {
        let dek = self.dek().map_err(|_| "本机尚未加入加密同步".to_string())?;
        let b = self.fresh_backend(None)?;
        let list = list_devices(&*b, &dek.devinfo_key()).map_err(|e| sync_error(&e))?;
        let before = self.state.group_name_at;
        adopt_group_name(&mut self.state, &list);
        if self.state.group_name_at != before {
            self.save_state();
        }
        let devices: Vec<Value> = list
            .iter()
            .map(|(id, info)| {
                let is_self = *id == self.state.device;
                match info {
                    Ok(i) => json!({
                        "id": id, "name": i.name, "platform": i.platform,
                        "app_version": i.app_version, "caps": i.caps,
                        "last_seen": i.last_seen, "is_self": is_self, "readable": true,
                        "group": i.group,
                        "group_name": wind_sync::devices::group_name_of(&list, &i.group)
                            .unwrap_or_default(),
                    }),
                    Err(_) => json!({
                        "id": id, "name": "", "platform": "", "app_version": "",
                        "caps": [], "last_seen": 0, "is_self": is_self, "readable": false,
                        "group": "", "group_name": "",
                    }),
                }
            })
            .collect();
        Ok(json!({ "devices": devices }))
    }

    /// 删除某设备在远端的清单与设备信息。它若仍在运行，下次同步会重新出现。
    pub(super) fn cmd_remove_device(&mut self, args: &Value) -> Result<Value, String> {
        if args.get("confirm").and_then(Value::as_bool) != Some(true) {
            return Err("移除设备须明确确认（confirm=true）".into());
        }
        let dev = str_arg(args, "dev")
            .filter(|d| !d.is_empty())
            .ok_or("缺少参数 dev")?;
        if dev == self.state.device {
            return Err("不能移除本机".into());
        }
        let b = self.fresh_backend(None)?;
        let removed = remove_device(&*b, dev).map_err(|e| sync_error(&e))?;
        log!("已移除设备 {dev}（{removed} 个对象）");
        Ok(json!({ "ok": true, "removed": removed }))
    }

    /// 清空远端后以本机为准重新上传。需 `confirm: true`；生成新的同步密钥并回执，其他设备须用新密钥重新加入。
    pub(super) fn cmd_reset_remote(&mut self, args: &Value) -> Result<Value, String> {
        if args.get("confirm").and_then(Value::as_bool) != Some(true) {
            return Err("重置远端会清空其中全部同步数据，须明确确认（confirm=true）".into());
        }
        let b = self.fresh_backend(None)?;
        let dek = Dek::generate();
        let prev = self.store_dek(&dek)?;
        if let Err(e) = remote::reset_remote_with(&*b, &dek) {
            self.rollback_dek(prev);
            return Err(sync_error(&e));
        }
        // 远端已空：旧基线作废，所有本地文件当作新增上传；本机就是新库的首台设备
        let mut fresh = SyncState::new(self.state.device.clone());
        fresh.group = group_of(&self.state).to_string();
        fresh.group_name = std::mem::take(&mut self.state.group_name);
        fresh.group_name_at = self.state.group_name_at;
        fresh.identity_confirmed = true;
        self.state = fresh;
        self.info_written = None;
        self.mark_verified(library_id(self.settings.backend.as_ref()));
        self.after_user_fix();
        self.publish_info(false);
        log!("远端已重置");
        Ok(json!({ "ok": true, "key": dek.to_key_string() }))
    }
}
