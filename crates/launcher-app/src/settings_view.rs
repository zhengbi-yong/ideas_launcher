//! The settings overlay.
//!
//! Flow Launcher opens a separate 940×600 settings window with panes for
//! General, Hotkey, Theme, Plugins, Plugin Store, Proxy and About. Here the
//! same sections live inside the launcher window as an overlay, which keeps a
//! single window to manage and makes the settings reachable from anywhere.
//!
//! The overlay is written in Chinese, because that is the language of the
//! people who use it. Every measure comes from the design tokens of the
//! family, so that this pane looks like the rest of the program — and like
//! its siblings.

use egui::{Color32, Rect, RichText, Stroke, Vec2};

use launcher_core::settings::{ColorScheme, FileSort, Settings};

use crate::theme::{metrics, Palette};

/// Group a number with thousands separators: 1234567 becomes "1,234,567".
fn thousands(value: usize) -> String {
    crate::app::format_thousands(value)
}

/// The level the log is written at.
///
/// The level is written by hand, so a word that is not one of the four levels
/// means `INFO` — a typo must not take the log away.
pub fn log_level_filter(text: &str) -> log::LevelFilter {
    match text.trim().to_ascii_uppercase().as_str() {
        "ERROR" => log::LevelFilter::Error,
        "WARN" => log::LevelFilter::Warn,
        "DEBUG" => log::LevelFilter::Debug,
        "TRACE" => log::LevelFilter::Trace,
        _ => log::LevelFilter::Info,
    }
}

/// What the index is doing, shown in the Indexing pane.
///
/// The pane cannot reach the index itself, so the window hands the state over
/// before the pane is drawn.
#[derive(Debug, Clone, Default)]
pub struct IndexStatus {
    /// Whether the full-disk index is being built.
    pub building: bool,
    /// How many entries the index holds so far.
    pub entries: usize,
    /// How far the work is done, between 0 and 1.
    ///
    /// `None` when the total is not known.
    pub fraction: Option<f32>,
    /// What is being read right now.
    pub phase: String,
    /// Why the full-disk index is not there.
    pub problem: Option<String>,
    /// Whether the launcher runs with administrative privileges.
    pub elevated: bool,
    /// How many entries the index of the user's own folders holds.
    pub live_entries: usize,
    /// How many entries the full-disk index holds.
    pub full_entries: usize,
}

/// The sections of the settings overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    /// General options.
    General,
    /// The theme picker.
    Theme,
    /// Search behaviour.
    Search,
    /// Program index and file search.
    Indexing,
    /// The query history.
    History,
    /// About the launcher.
    About,
}

impl SettingsSection {
    /// The label shown in the navigation list.
    pub fn label(self) -> &'static str {
        match self {
            SettingsSection::General => "通用",
            SettingsSection::Theme => "主题",
            SettingsSection::Search => "搜索",
            SettingsSection::Indexing => "索引",
            SettingsSection::History => "历史",
            SettingsSection::About => "关于",
        }
    }

    /// The icon glyph for the navigation list.
    pub fn glyph(self) -> &'static str {
        match self {
            SettingsSection::General => "\u{e713}",
            SettingsSection::Theme => "\u{e790}",
            SettingsSection::Search => "\u{e721}",
            SettingsSection::Indexing => "\u{e7c3}",
            SettingsSection::History => "\u{e81c}",
            SettingsSection::About => "\u{e946}",
        }
    }

    /// Every section, in display order.
    pub fn all() -> Vec<Self> {
        vec![
            SettingsSection::General,
            SettingsSection::Theme,
            SettingsSection::Search,
            SettingsSection::Indexing,
            SettingsSection::History,
            SettingsSection::About,
        ]
    }
}

/// The settings overlay.
pub struct SettingsView {
    section: SettingsSection,
    themes: Vec<String>,
    pending_close: bool,
    pending_theme: Option<String>,
    new_theme: String,
    /// The hotkey that really works right now.
    ///
    /// It can be another one than the configured one: when the combination is
    /// taken by another program, the launcher falls back to something else.
    effective_hotkey: String,
    /// The user asked for the full-disk index to be rebuilt.
    pending_rebuild: bool,
    /// The user asked for the launcher to start with administrative
    /// privileges.
    pending_elevate: bool,
    /// The user asked for the index service to be installed.
    pending_install_service: bool,
    /// What the index is doing right now.
    status: IndexStatus,
}

impl SettingsView {
    /// Create the overlay.
    pub fn new() -> Self {
        Self {
            section: SettingsSection::General,
            themes: Vec::new(),
            pending_close: false,
            pending_theme: None,
            new_theme: String::new(),
            effective_hotkey: String::new(),
            pending_rebuild: false,
            pending_elevate: false,
            pending_install_service: false,
            status: IndexStatus::default(),
        }
    }

    /// True when the user asked for the full-disk index to be rebuilt.
    ///
    /// The pane cannot reach the index itself, so it leaves the request here
    /// and the window picks it up.
    pub fn take_rebuild_request(&mut self) -> bool {
        std::mem::take(&mut self.pending_rebuild)
    }

    /// True when the user asked for the index service to be installed.
    ///
    /// The service holds the index of the whole machine, so the launcher
    /// does not have to read the disk itself.
    pub fn take_install_service_request(&mut self) -> bool {
        std::mem::take(&mut self.pending_install_service)
    }

    /// True when the user asked for an elevated copy of the launcher.
    ///
    /// Reading the master file table needs administrative privileges, so the
    /// pane offers to start the launcher with them.
    pub fn take_elevate_request(&mut self) -> bool {
        std::mem::take(&mut self.pending_elevate)
    }

    /// Tell the panel what the index is doing.
    pub fn set_index_status(&mut self, status: IndexStatus) {
        self.status = status;
    }

    /// Tell the panel which hotkey really works.
    pub fn set_effective_hotkey(&mut self, hotkey: &str) {
        self.effective_hotkey = hotkey.to_owned();
    }

    /// Provide the list of selectable themes.
    pub fn set_theme_list(&mut self, themes: Vec<String>) {
        if self.new_theme.is_empty() {
            self.new_theme = themes.first().cloned().unwrap_or_default();
        }
        self.themes = themes;
    }

    /// The section currently shown.
    pub fn section(&self) -> SettingsSection {
        self.section
    }

    /// Whether the overlay asked to be closed.
    pub fn should_close(&self) -> bool {
        self.pending_close
    }

    /// Clear the close request.
    pub fn request_close(&mut self) {
        self.pending_close = false;
    }

    /// Take a theme change, if one was requested.
    pub fn take_theme_change(&mut self) -> Option<String> {
        self.pending_theme.take()
    }

    /// Draw the overlay.
    pub fn show(&mut self, ui: &mut egui::Ui, settings: &mut Settings, palette: &mut Palette) {
        // The overlay fills the whole window, with square corners. Anything
        // else would put a second shape next to the window's own.
        let full = ui.max_rect();
        let painter = ui.painter().clone();

        // Backdrop.
        painter.rect_filled(full, egui::CornerRadius::ZERO, palette.surface);

        let nav_width = metrics::NAV_WIDTH;
        let nav_rect = Rect::from_min_size(full.min, Vec2::new(nav_width, full.height()));
        painter.rect_filled(
            nav_rect,
            egui::CornerRadius::ZERO,
            palette.surface_alt,
        );

        // ---- navigation -------------------------------------------------
        let mut nav_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(nav_rect.shrink(8.0))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        nav_ui.spacing_mut().item_spacing.y = 2.0;

        nav_ui.add_space(4.0);
        nav_ui.label(
            RichText::new("设置")
                .color(palette.text)
                .size(metrics::TEXT_TITLE)
                .strong(),
        );
        nav_ui.add_space(8.0);

        for section in SettingsSection::all() {
            let selected = section == self.section;
            let text = RichText::new(section.label())
                .color(if selected { palette.text } else { palette.text_dim })
                .size(metrics::TEXT_BODY);

            let response = nav_ui.add(
                egui::Button::new(text)
                    .selected(selected)
                    .fill(if selected {
                        palette.accent_soft
                    } else {
                        Color32::TRANSPARENT
                    })
                    .stroke(Stroke::NONE)
                    .min_size(Vec2::new(nav_width - 24.0, metrics::ITEM_HEIGHT)),
            );
            if response.clicked() {
                self.section = section;
            }
        }

        nav_ui.add_space(10.0);
        if nav_ui.button("关闭  (Esc)").clicked() {
            self.pending_close = true;
        }

        // ---- content ----------------------------------------------------
        let content_rect = Rect::from_min_size(
            egui::pos2(nav_rect.right() + 8.0, full.top() + 8.0),
            Vec2::new(
                (full.width() - nav_width - 24.0).max(120.0),
                (full.height() - 16.0).max(80.0),
            ),
        );
        let mut content = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(content_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        // The content is taller than the window on some panes — the indexing
        // pane, for one, has more in it than fits. Without a scroll area the
        // buttons at the bottom would be out of reach.
        egui::ScrollArea::vertical()
            .id_salt("settings-content")
            .max_height(content_rect.height())
            .auto_shrink([false, false])
            .show(&mut content, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;

                match self.section {
                    SettingsSection::General => self.general(ui, settings, palette),
                    SettingsSection::Theme => self.theme_section(ui, settings, palette),
                    SettingsSection::Search => self.search(ui, settings, palette),
                    SettingsSection::Indexing => self.indexing(ui, settings, palette),
                    SettingsSection::History => self.history(ui, settings, palette),
                    SettingsSection::About => self.about(ui, settings, palette),
                }
            });
    }

    /// The General pane.
    fn general(&mut self, ui: &mut egui::Ui, settings: &mut Settings, palette: &Palette) {
        heading(ui, "通用", palette);

        ui.label(label("热键", palette));
        let mut hotkey = settings.hotkey.clone();
        if ui
            .add(
                egui::TextEdit::singleline(&mut hotkey)
                    .desired_width(220.0)
                    .hint_text("Alt + Space"),
            )
            .changed()
        {
            settings.hotkey = hotkey;
        }
        // Which hotkey really works. It can be another one than the
        // configured one, and the user has to know which to press.
        if self.effective_hotkey.is_empty() {
            ui.label(hint("没有任何热键注册成功 —— 所有备选组合都被占用了。", palette));
        } else {
            let taken = self.effective_hotkey != settings.hotkey;
            let text = if taken {
                format!(
                    "该组合已被其它程序占用，当前生效的是 {}。",
                    self.effective_hotkey
                )
            } else {
                format!("当前生效：{}。", self.effective_hotkey)
            };
            ui.label(hint(&text, palette));
        }

        ui.add_space(6.0);
        ui.label(label("配色方案", palette));
        egui::ComboBox::from_label("")
            .selected_text(match settings.color_scheme {
                ColorScheme::System => "跟随系统",
                ColorScheme::Light => "浅色",
                ColorScheme::Dark => "深色",
            })
            .show_ui(ui, |ui| {
                for (scheme, text) in [
                    (ColorScheme::System, "跟随系统"),
                    (ColorScheme::Light, "浅色"),
                    (ColorScheme::Dark, "深色"),
                ] {
                    ui.selectable_value(&mut settings.color_scheme, scheme, text);
                }
            });

        ui.add_space(6.0);
        ui.label(label("界面语言", palette));
        // The interface is written in Chinese. There is no second language
        // yet, so this is a statement and not a choice — a switch that does
        // nothing is worse than no switch at all.
        ui.label(hint("简体中文", palette));

        ui.add_space(6.0);
        checkbox(ui, "显示时钟", &mut settings.use_clock, palette);
        checkbox(ui, "显示日期", &mut settings.use_date, palette);
        checkbox(ui, "打开时播放提示音", &mut settings.use_sound, palette);
        checkbox(ui, "窗口动画", &mut settings.use_animation, palette);
        checkbox(ui, "预览时读取图片和视频", &mut settings.preview_media, palette);
        ui.label(hint(
            "打开时会把整个文件从盘上读进来。关着的时候，预览只显示索引里已经有的东西（大小、修改时间）。",
            palette,
        ));
        checkbox(
            ui,
            "始终在任务栏显示托盘图标",
            &mut settings.always_show_tray_icon,
            palette,
        );
        ui.label(hint(
            "关闭时图标收进任务栏的隐藏区，用热键即可唤出。",
            palette,
        ));
    }

    /// The Theme pane.
    fn theme_section(&mut self, ui: &mut egui::Ui, settings: &mut Settings, palette: &Palette) {
        heading(ui, "主题", palette);
        ui.label(hint(
            "挑一个内置主题。启动器自己画卡片，不需要任何额外运行时。",
            palette,
        ));
        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .max_height(240.0)
            .show(ui, |ui| {
                for name in self.themes.clone() {
                    let selected = name == settings.theme;
                    let text = RichText::new(&name)
                        .color(if selected { palette.text } else { palette.text_dim })
                        .size(metrics::TEXT_BODY);
                    if ui
                        .add(
                            egui::Button::new(text)
                                .selected(selected)
                                .fill(if selected {
                                    palette.accent_soft
                                } else {
                                    Color32::TRANSPARENT
                                })
                                .stroke(Stroke::NONE)
                                .min_size(Vec2::new(220.0, 26.0)),
                        )
                        .clicked()
                    {
                        settings.theme = name.clone();
                        self.pending_theme = Some(name.clone());
                    }
                }
            });
    }

    /// The Search pane.
    fn search(&mut self, ui: &mut egui::Ui, settings: &mut Settings, palette: &Palette) {
        heading(ui, "搜索", palette);

        ui.label(label("最多显示的结果数", palette));
        let mut max_results = settings.max_results_to_show as i32;
        if ui
            .add(
                egui::Slider::new(&mut max_results, 100..=10_000)
                    .suffix(" 条")
                    .logarithmic(true),
            )
            .changed()
        {
            settings.max_results_to_show = max_results.max(100) as usize;
        }
        ui.label(hint(
            "结果不是只有窗口里看得见的那十几行 —— 列表里存着这么多,\
             往下翻就能翻到排在后面的那一条。",
            palette,
        ));

        ui.add_space(6.0);
        ui.label(label("结果的顺序", palette));
        ui.label(hint(
            "默认把可运行的程序放在最上面,文件排在后面 —— 启动器大多数时候是用来开程序的。",
            palette,
        ));
        egui::ComboBox::from_id_salt("result-order")
            .selected_text(settings.result_order.label())
            .show_ui(ui, |ui| {
                for option in launcher_core::ResultOrder::all() {
                    ui.selectable_value(
                        &mut settings.result_order,
                        *option,
                        option.label(),
                    );
                }
            });

        ui.add_space(6.0);
        ui.label(label("匹配精度", palette));
        egui::ComboBox::from_label("")
            .selected_text(match settings.query_search_precision {
                launcher_core::SearchPrecision::Regular => "常规",
                launcher_core::SearchPrecision::Low => "宽松",
                launcher_core::SearchPrecision::None => "显示全部",
            })
            .show_ui(ui, |ui| {
                use launcher_core::SearchPrecision::*;
                for (value, text) in [
                    (Regular, "常规"),
                    (Low, "宽松"),
                    (None, "显示全部"),
                ] {
                    ui.selectable_value(
                        &mut settings.query_search_precision,
                        value,
                        text,
                    );
                }
            });

        ui.add_space(6.0);
        checkbox(ui, "用拼音搜索中文", &mut settings.should_use_pinyin, palette);
        checkbox(ui, "总是以英文开始", &mut settings.always_start_en, palette);

        ui.add_space(6.0);
        checkbox(
            ui,
            "从我选过的结果中学习",
            &mut settings.enable_smart_ranking,
            palette,
        );
        if settings.enable_smart_ranking {
            ui.label(hint("同一条结果被选得越多，就排得越靠前。", palette));
            let mut threshold = settings.pick_count_threshold as i32;
            if ui
                .add(egui::Slider::new(&mut threshold, 1..=10).suffix(" 次"))
                .changed()
            {
                settings.pick_count_threshold = threshold.clamp(1, 10) as u32;
            }
        }

        ui.add_space(6.0);
        ui.label(label("搜索框字号", palette));
        let mut size = settings.query_box_font_size as i32;
        if ui
            .add(egui::Slider::new(&mut size, 10..=28).suffix(" 磅"))
            .changed()
        {
            settings.query_box_font_size = size.max(10) as u32;
        }
    }

    /// The Indexing pane.
    fn indexing(&mut self, ui: &mut egui::Ui, settings: &mut Settings, palette: &Palette) {
        heading(ui, "索引", palette);
        ui.label(hint(
            "程序索引和文件索引都在后台建立，不会卡住窗口。",
            palette,
        ));
        ui.add_space(6.0);

        ui.label(label("索引的文件夹", palette));
        ui.label(hint(
            "默认索引桌面、文档、下载、图片、音乐和视频。",
            palette,
        ));

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(4.0);

        heading(ui, "全盘索引", palette);
        ui.label(hint(
            "读取 NTFS 的主文件表，可以搜到整台机器上的每一个文件。\
             这一步需要管理员权限，没有权限时会退回文件夹搜索。",
            palette,
        ));
        ui.add_space(6.0);

        checkbox(
            ui,
            "启用全盘索引",
            &mut settings.enable_full_disk_index,
            palette,
        );
        checkbox(
            ui,
            "以管理员身份运行",
            &mut settings.run_as_administrator,
            palette,
        );
        ui.label(hint(
            "启动的时候自动申请管理员权限 —— 全盘索引要读 NTFS 的主文件表，没有权限读不到。\
             关掉它，程序就只搜用户目录（桌面、文档、下载……）。",
            palette,
        ));
        if settings.enable_full_disk_index {
            ui.label(label("索引的卷", palette));
            ui.label(hint(
                "留空表示所有 NTFS 卷。用逗号分隔，例如 C,D。",
                palette,
            ));
            let mut volumes = settings.indexed_volumes.join(",");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut volumes)
                        .desired_width(160.0)
                        .hint_text("C,D"),
                )
                .changed()
            {
                settings.indexed_volumes = split_list(&volumes);
            }

            ui.label(label("排除的文件夹", palette));
            ui.label(hint("用逗号分隔，例如 D:\\Downloads,D:\\Temp。", palette));
            let mut excluded = settings.excluded_paths.join(",");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut excluded)
                        .desired_width(260.0)
                        .hint_text("D:\\Downloads"),
                )
                .changed()
            {
                settings.excluded_paths = split_list(&excluded);
            }

            ui.label(label("排除的扩展名", palette));
            ui.label(hint("用逗号分隔，例如 tmp,iso。", palette));
            let mut extensions = settings.excluded_extensions.join(",");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut extensions)
                        .desired_width(160.0)
                        .hint_text("tmp,iso"),
                )
                .changed()
            {
                settings.excluded_extensions = split_list(&extensions);
            }

            ui.label(label("索引的更新间隔", palette));
            ui.label(hint(
                "秒。文件系统的变化**按回合**进索引,不是来一个就动一次 —— 一个程序写一万个文件,\
                 索引只被改一轮,盘也只被读一轮。新建的文件晚这么一会儿才找得到,\
                 但通常没有人会在刚建完文件的那一刻就去找它。",
                palette,
            ));
            let mut interval = settings.index_update_interval as i32;
            if ui
                .add(egui::Slider::new(&mut interval, 5..=600).suffix(" 秒"))
                .changed()
            {
                settings.index_update_interval = interval.clamp(5, 600) as u64;
            }

            ui.label(label("结果的排序", palette));
            egui::ComboBox::from_id_salt("file-sort")
                .selected_text(settings.file_sort.label())
                .show_ui(ui, |ui| {
                    for option in FileSort::all() {
                        ui.selectable_value(
                            &mut settings.file_sort,
                            option,
                            option.label(),
                        );
                    }
                });

            ui.label(label("缓存的有效期", palette));
            ui.label(hint(
                "小时。超过这个时间就重新读一遍主文件表 —— 读一遍要十几秒，但只写真正变了的那几个文件。",
                palette,
            ));
            let mut hours = settings.full_disk_index_max_age_hours as i32;
            if ui
                .add(egui::Slider::new(&mut hours, 0..=720).suffix(" 小时"))
                .changed()
            {
                settings.full_disk_index_max_age_hours = hours.clamp(0, 720) as u64;
            }
            if hours == 0 {
                ui.label(hint(
                    "0 表示不再自动重建：索引会一直用着，要新的就点下面的按钮。",
                    palette,
                ));
            }

            if ui.button("重建全盘索引").clicked() {
                self.pending_rebuild = true;
            }
            ui.label(hint(
                "重建会重新读一遍主文件表，可能要几分钟。旧索引会一直用到新的建好。",
                palette,
            ));

            ui.add_space(10.0);
            ui.label(label("索引服务", palette));
            ui.label(hint(
                "把索引放进一个系统服务：开机就在，窗口不用再读磁盘，内存也能降下来。",
                palette,
            ));
            if ui.button("安装索引服务").clicked() {
                self.pending_install_service = true;
            }

            // The progress of the build, so that the button does not look like
            // it did nothing at all.
            if self.status.building {
                let count = thousands(self.status.entries);
                let phase = if self.status.phase.is_empty() {
                    "正在建立文件索引".to_string()
                } else {
                    self.status.phase.clone()
                };
                let text = format!("{phase}… {count} 项");
                ui.add(
                    egui::ProgressBar::new(self.status.fraction.unwrap_or(0.0))
                        .desired_width(320.0)
                        .text(text),
                );
            }

            // The reason why the full-disk index is not there.
            if let Some(problem) = self.status.problem.clone() {
                ui.label(hint(&problem, palette));
                if !self.status.elevated {
                    ui.label(hint(
                        "读取主文件表需要管理员权限。用管理员身份重新启动后，再点一次重建。",
                        palette,
                    ));
                    if ui.button("以管理员身份重新启动…").clicked() {
                        self.pending_elevate = true;
                    }
                }
            }

            // The index of the user's own folders works without privileges.
            //
            // When the full-disk index is there and keeps itself up to date,
            // the folders of the user are **not** walked through a second
            // time — they are covered by it, and there is nothing to count.
            let live = thousands(self.status.live_entries);
            if self.status.live_entries > 0 {
                ui.label(hint(
                    &format!("用户目录索引 {live} 项（实时更新，不需要管理员权限）"),
                    palette,
                ));
            } else if self.status.full_entries > 0 {
                ui.label(hint("用户目录由全盘索引覆盖（实时更新）", palette));
            }
            if self.status.full_entries > 0 {
                let full = thousands(self.status.full_entries);
                ui.label(hint(&format!("全盘索引 {full} 项"), palette));
            }
        }

        ui.add_space(10.0);
        ui.label(label("日志级别", palette));
        let mut level = settings.log_level.clone();
        if ui
            .add(
                egui::TextEdit::singleline(&mut level)
                    .desired_width(120.0)
                    .hint_text("INFO"),
            )
            .changed()
        {
            settings.log_level = level.clone();
            // The level takes effect at once — a log that is too quiet is
            // noticed right away, and a restart would be in the way.
            log::set_max_level(log_level_filter(&level));
        }
        ui.label(hint(
            "ERROR / WARN / INFO / DEBUG / TRACE（默认 INFO）",
            palette,
        ));
    }

    /// The History pane.
    fn history(&mut self, ui: &mut egui::Ui, settings: &mut Settings, palette: &Palette) {
        heading(ui, "历史", palette);
        ui.label(hint(
            "执行过的查询会被记下来，可以来回翻。",
            palette,
        ));
        ui.add_space(6.0);
        ui.label(hint(
            &format!("历史文件：{}", crate::paths::history_file().display()),
            palette,
        ));
        let _ = settings;
    }

    /// The About pane.
    fn about(&mut self, ui: &mut egui::Ui, settings: &mut Settings, palette: &Palette) {
        heading(ui, "关于", palette);
        ui.label(
            RichText::new("IDEAS Launcher")
                .color(palette.text)
                .size(18.0)
                .strong(),
        );
        ui.label(
            RichText::new("用 Rust 写的快速启动器。")
                .color(palette.text_dim)
                .size(metrics::TEXT_BODY),
        );
        ui.add_space(8.0);
        ui.label(hint(
            &format!("数据目录：{}", crate::paths::data_directory().display()),
            palette,
        ));
        ui.label(hint(
            &format!("版本：{}", env!("CARGO_PKG_VERSION")),
            palette,
        ));
        let _ = settings;
    }
}

impl Default for SettingsView {
    fn default() -> Self {
        Self::new()
    }
}

/// Draw a section heading.
fn heading(ui: &mut egui::Ui, text: &str, palette: &Palette) {
    ui.label(
        RichText::new(text)
            .color(palette.text)
            .size(metrics::TEXT_TITLE)
            .strong(),
    );
    ui.add_space(2.0);
    ui.separator();
    ui.add_space(4.0);
}

/// The name of a setting.
fn label(text: &str, palette: &Palette) -> RichText {
    RichText::new(text)
        .color(palette.text_dim)
        .size(metrics::TEXT_LABEL)
}

/// A hint, a path, a footnote.
fn hint(text: &str, palette: &Palette) -> RichText {
    RichText::new(text)
        .color(palette.text_faint)
        .size(metrics::TEXT_CAPTION)
}

/// Split a comma separated list into its parts.
fn split_list(text: &str) -> Vec<String> {
    text.split([',', ';', '，', '；'])
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

/// Draw a labelled check box.
fn checkbox(ui: &mut egui::Ui, text: &str, value: &mut bool, palette: &Palette) {    ui.add(
        egui::Checkbox::new(
            value,
            egui::RichText::new(text).color(palette.text),
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_has_a_label_and_a_glyph() {
        for section in SettingsSection::all() {
            assert!(!section.label().is_empty());
            assert!(!section.glyph().is_empty());
        }
    }

    #[test]
    fn the_log_level_is_read_from_what_the_user_wrote() {
        assert_eq!(log_level_filter("DEBUG"), log::LevelFilter::Debug);
        assert_eq!(log_level_filter("debug"), log::LevelFilter::Debug);
        assert_eq!(log_level_filter("  Debug "), log::LevelFilter::Debug);
        assert_eq!(log_level_filter("WARN"), log::LevelFilter::Warn);
        assert_eq!(log_level_filter("ERROR"), log::LevelFilter::Error);
        assert_eq!(log_level_filter("TRACE"), log::LevelFilter::Trace);
        // A word that is not a level must not take the log away.
        assert_eq!(log_level_filter(""), log::LevelFilter::Info);
        assert_eq!(log_level_filter("VERBOSE"), log::LevelFilter::Info);
    }

    #[test]
    fn the_sections_are_written_in_chinese() {
        // The whole interface is Chinese, and it has to stay that way.
        for section in SettingsSection::all() {
            assert!(
                section.label().chars().any(|c| c as u32 > 0x2E80),
                "{} is not Chinese",
                section.label()
            );
        }
    }

    #[test]
    fn theme_change_is_taken_once() {
        let mut view = SettingsView::new();
        view.pending_theme = Some("Dracula".into());
        assert_eq!(view.take_theme_change().as_deref(), Some("Dracula"));
        assert!(view.take_theme_change().is_none());
    }

    #[test]
    fn close_request_is_consumed_once() {
        let mut view = SettingsView::new();
        view.pending_close = true;
        assert!(view.should_close());
        view.request_close();
        assert!(!view.should_close());
    }
}
