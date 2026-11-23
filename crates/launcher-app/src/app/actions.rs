use super::*;

impl LauncherApp {
    pub(super) fn open_selected(&mut self) {
        let Some(result) = self.results.get(self.selected).cloned() else {
            return;
        };
        self.open_result(&result);
    }

    /// Open one result, whatever route it was picked by.
    ///
    /// The keyboard and the mouse both end up here.
    pub(super) fn open_result(&mut self, result: &LauncherResult) {
        let query = self.query_text.clone();
        let index = self.selected;

        // Remember the choice. The row is counted by its identity, so the
        // learning survives the list being in a different order the next
        // time — and a row that is picked often enough climbs up on its own.
        let item = launcher_core::history::item_key(
            &result.plugin_id,
            &result.title,
            &result.subtitle,
        );
        self.selection.record_pick(&query, item);
        self.selection.record(&query, index);
        self.history.record(&query);

        // The deeper details of a file — its index number, its streams, what
        // opens it, how many things a folder holds — are read **here**, when
        // the item is opened, and not while the list is being browsed.
        //
        // Browsing the list must not touch the disk: every row the pointer
        // crosses would open a file, and the machine would be busy the whole
        // time the pointer moves.
        if result.plugin_id == "file" {
            let path = if result.copy_text.is_empty() {
                result.subtitle.clone()
            } else {
                result.copy_text.clone()
            };
            if !path.is_empty() {
                // This queues the reading on a thread of its own; it does not
                // hold up the opening.
                self.details.get(std::path::Path::new(&path));
            }
        }

        // The icon of the program is taken at the same moment: the file is
        // being opened anyway, so that is the moment to read its icon — and
        // it is kept, so that the next time this program comes up, its row
        // has an icon without reading anything.
        if let launcher_core::result::ResultIcon::Path(icon) = &result.icon {
            self.icons.learn(icon);
        }

        let target = if result.copy_text.is_empty() {
            result.title.clone()
        } else {
            result.copy_text.clone()
        };

        match result.plugin_id.as_str() {
            "calculator" => {
                icons::shell_open_with("clip://", &target);
            }
            "shell" => {
                let mut cmd = String::from("cmd /c ");
                cmd.push_str(&target);
                icons::shell_run_console("cmd.exe", &cmd);
            }
            "process" => {
                if let Ok(pid) = target.parse::<u32>() {
                    if let Err(e) = plugins::processes::kill(pid) {
                        log::warn!("could not kill process {pid}: {e}");
                    }
                }
            }
            "sys" => {
                if let Some(action) = sys_action_for(&result.title) {
                    if let Err(e) = plugins::sys::execute(action) {
                        log::warn!("system command failed: {e}");
                    }
                }
            }
            "uwp" => {
                // The Store applications are not files: they are started by
                // their id, through the shell of the installed applications.
                let aumid = result.context_data.clone().unwrap_or_default();
                if aumid.is_empty() {
                    log::warn!("the Store application has no id: {:?}", result.title);
                } else {
                    icons::shell_open(&format!("shell:AppsFolder\\{aumid}"));
                }
            }
            "windows_settings" => {
                // The command of the row opens the page; half of them are
                // control-panel commands that the shell cannot resolve on
                // its own.
                if !plugins::windows_settings::open_settings(&target) {
                    log::warn!("could not open the settings page: {target}");
                }
            }
            _ => {
                // An external plugin acts on its own results.
                if self
                    .json_rpc_plugins
                    .iter()
                    .any(|plugin| plugin.id() == result.plugin_id)
                {
                    let hide = self.engine.execute(&result);
                    if hide {
                        self.hide();
                    }
                    return;
                }
                icons::shell_open(&target);
            }
        }

        self.hide();
    }

    /// Write the settings to disk.
    ///
    /// The settings panel changes the settings in place, so they are written
    /// when the panel is closed and when the theme changes.
    pub(super) fn save_settings(&mut self) {
        if let Err(e) = self.settings.save(&crate::paths::settings_file()) {
            log::error!("could not write the settings: {e}");
        }
        // The ranking learns with a threshold; a new one takes effect at once.
        self.selection.set_threshold(if self.settings.enable_smart_ranking {
            self.settings.pick_count_threshold
        } else {
            0
        });
        // The settings of the full-disk index are applied at once, so that a
        // changed exclusion or a changed volume shows up right away.
        self.file_plugin.configure(&self.settings);
        if !self.settings.enable_full_disk_index && self.file_plugin.has_full_index() {
            log::info!("the full-disk index was switched off");
            self.file_plugin.clear_cache();
        }
        // The order of the results is applied at once as well.
        self.engine.set_result_order(self.settings.result_order);
        // The number of results the list holds, too: the user asked for
        // more, and the list is scrolled through.
        self.engine.set_max_results(self.settings.max_results_to_show);
    }

    /// Copy the selected result to the clipboard.
    pub(super) fn copy_selected(&mut self) {
        let Some(result) = self.results.get(self.selected) else {
            return;
        };
        let text = if result.copy_text.is_empty() {
            result.title.clone()
        } else {
            result.copy_text.clone()
        };
        if text.is_empty() {
            return;
        }
        self.ctx.copy_text(text.clone());
        log::info!("copied {text:?} to the clipboard");
    }

    /// Complete the query with the selected result.
    pub(super) fn complete_selected(&mut self) {
        let Some(result) = self.results.get(self.selected) else {
            return;
        };
        let completion = if result.auto_complete_text.is_empty() {
            result.title.clone()
        } else {
            result.auto_complete_text.clone()
        };
        if completion.is_empty() || completion == self.query_text {
            return;
        }
        self.query_text = completion;
        self.run_query();
    }

    /// Move the selection by `delta`, wrapping around.
    pub(super) fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            return;
        }
        let len = self.results.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(len);
        self.selected = next as usize;
        self.sync_scroll();
    }

    /// Keep the selected row inside the visible area.
    ///
    /// The height of the list is the one the window actually has. It used to
    /// be a fixed number, and a window that grew past it made the list jump
    /// up over the search box.
    ///
    /// The list itself is scrolled by `egui`, and the offset it ends up with
    /// is read back after every frame. This function only asks for a jump:
    /// writing the offset every frame would throw away what the user did with
    /// the wheel or with the scroll bar.
    pub(super) fn sync_scroll(&mut self) {
        let row = row_pitch();
        let target = self.selected as f32 * row;
        let height = if self.list_height > 0.0 {
            self.list_height
        } else {
            320.0
        };
        let mut wanted = self.scroll_offset;
        if target < wanted {
            wanted = target;
        } else if target + row > wanted + height {
            wanted = target + row - height;
        }
        if (wanted - self.scroll_offset).abs() > 0.5 {
            self.scroll_offset = wanted;
            self.pending_scroll = Some(wanted);
        }
    }


    /// Remember that a dialog of the system was opened.
    ///
    /// The window keeps itself for a while, even without the focus: the
    /// dialog took the focus away from it, and it needs a moment to come up.
    pub(super) fn system_dialog_opened(&mut self) {
        self.system_dialog_until = Some(Instant::now() + Duration::from_secs(8));
    }

    /// Give the preview pane a player for the video that is looked at.
    ///
    /// The player belongs to one file: when another file is looked at, it is
    /// let go — a decoder that keeps running is a decoder that keeps the
    /// disk busy.
    pub(super) fn prepare_video(&mut self, content: &preview::PreviewContent) {
        let preview::PreviewContent::File { path, video, .. } = content else {
            self.video = None;
            self.pending_video = None;
            return;
        };
        if !*video {
            self.video = None;
            self.pending_video = None;
            return;
        }
        let same = self
            .video
            .as_ref()
            .map(|player| player.path() == path)
            .unwrap_or(false);
        if same {
            return;
        }
        // The player is not started right away: the pointer moves through
        // the list quickly, and every video it passes would start a
        // decoder of its own.
        let waiting = self
            .pending_video
            .as_ref()
            .map(|(other, _)| other == path)
            .unwrap_or(false);
        if !waiting {
            self.pending_video = Some((path.clone(), Instant::now()));
        }
    }

    /// Give the video that is being looked at its player.
    ///
    /// The player waits a moment: the pointer chooses the row it is over,
    /// and a row that is only passed over is not being looked at. Without
    /// the wait, a sweep through a list of videos starts a decoder for
    /// every one of them.
    pub(super) fn start_pending_video(&mut self) {
        let Some((path, since)) = self.pending_video.clone() else {
            return;
        };
        if since.elapsed() < Duration::from_millis(150) {
            return;
        }
        self.pending_video = None;
        // What the details pane already found out about the file is handed
        // over, so that the player does not have to ask FFmpeg about it a
        // second time.
        let known = self
            .details
            .get_cached(std::path::Path::new(&path))
            .and_then(|details| details.video.clone());
        self.video = Some(video::VideoPlayer::new(
            std::path::Path::new(&path),
            self.ctx.clone(),
            known,
        ));
    }

    /// Apply a theme change coming from the settings overlay.
    pub(super) fn apply_theme(&mut self, name: &str) {
        self.settings.theme = name.to_string();
        self.palette = palette_for(&self.settings);
        crate::theme::apply_visuals(&self.ctx, &self.palette);
    }
}
