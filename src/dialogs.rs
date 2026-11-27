//! 对话框层：地址簿、设置（含按键宏与代理）、站点/代理编辑。
//!
//! eframe 0.37 里 App::ui 收到的是根 Ui，面板用 `Panel::top` 挂在根 Ui 上；
//! 对话框仍用 `egui::Window::show(&ctx, ...)`。
//!
//! 关键：窗口打开期间不得无条件重置 app.dlg，否则窗口只闪现一帧。
//! 只在用户明确操作（点 X、取消、保存、连接）时才切换 dlg。

use crate::config::{self, CharEncoding, InputConv, Macro, Proxy, Protocol, Site};
use crate::gui::App;
use egui::Id;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dlg {
    None,
    AddressBook,
    Settings,
    /// 编辑 cfg.sites 中第 N 项
    Site(usize),
    /// 新建站点
    SiteNew,
    /// 编辑 cfg.proxies 中第 N 项
    Proxy(usize),
    /// 新建代理
    ProxyNew,
    /// SSH 连接前询问用户名/密码
    SshAuth,
}

/// 由界面菜单调用：把某个站点载入编辑器。idx 为 cfg.sites 中的下标。
pub fn edit_site(app: &mut App, site: Site, idx: Option<usize>) {
    app.edit_original_name = Some(site.name.clone());
    app.draft_site = site;
    app.dlg = match idx {
        Some(i) => Dlg::Site(i),
        None => Dlg::SiteNew,
    };
}

pub fn draw(ctx: &egui::Context, app: &mut App) {
    match app.dlg {
        Dlg::None => {}
        Dlg::AddressBook => address_book(ctx, app),
        Dlg::Settings => settings(ctx, app),
        Dlg::Site(i) => site_editor(ctx, app, Some(i)),
        Dlg::SiteNew => site_editor(ctx, app, None),
        Dlg::Proxy(i) => proxy_editor(ctx, app, Some(i)),
        Dlg::ProxyNew => proxy_editor(ctx, app, None),
        Dlg::SshAuth => ssh_auth(ctx, app),
    }
}

// ------------------------------------------------------------ 地址簿

fn address_book(ctx: &egui::Context, app: &mut App) {
    let mut open = true;
    let mut to_connect: Option<Site> = None;
    let mut to_edit: Option<usize> = None;
    let mut del_group: Option<String> = None;

    // 分组顺序：先 cfg.groups，再补站点中出现但不在列表里的分组
    let mut order: Vec<String> = app.cfg.groups.clone();
    for s in &app.cfg.sites {
        let g = if s.group.is_empty() { "未分组".to_string() } else { s.group.clone() };
        if !order.contains(&g) {
            order.push(g);
        }
    }
    if !order.contains(&"未分组".to_string()) {
        order.push("未分组".to_string());
    }
    // 每个分组下的站点（带全局下标）
    let groups: Vec<(String, Vec<(usize, Site)>)> = order
        .iter()
        .map(|g| {
            let list: Vec<(usize, Site)> = app
                .cfg
                .sites
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    if g == "未分组" { s.group.is_empty() } else { &s.group == g }
                })
                .map(|(i, s)| (i, s.clone()))
                .collect();
            (g.clone(), list)
        })
        .collect();

    egui::Window::new("地址簿")
        .id(Id::new("dlg_addrbook"))
        .open(&mut open)
        .collapsible(false)
        .default_size([660.0, 480.0])
        // 锚定 ATerm 界面正中
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("＋ 新建分组").clicked() {
                    app.show_group_input = true;
                }
                ui.label(format!("共 {} 个站点", app.cfg.sites.len()));
            });
            // 新建分组输入行（用持久状态，跨帧不丢失）
            if app.show_group_input {
                ui.horizontal(|ui| {
                    ui.label("分组名");
                    ui.add(
                        egui::TextEdit::singleline(&mut app.draft_group)
                            .desired_width(180.0)
                            .hint_text("输入新分组名称"),
                    );
                    if ui.button("确定").clicked() {
                        let g = app.draft_group.trim().to_string();
                        if !g.is_empty() && g != "未分组" && !app.cfg.groups.contains(&g) {
                            app.cfg.groups.push(g);
                            app.save_cfg();
                        }
                        app.draft_group.clear();
                        app.show_group_input = false;
                    }
                    if ui.button("取消").clicked() {
                        app.draft_group.clear();
                        app.show_group_input = false;
                    }
                });
            }
            ui.separator();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (g, list) in &groups {
                        ui.horizontal(|ui| {
                            ui.collapsing(format!("{g}（{}）", list.len()), |ui| {
                                for (si, s) in list {
                                    ui.horizontal(|ui| {
                                        let label = format!(
                                            "{:<20} {:<26} {:>5}  {}  {}",
                                            s.name, s.host, s.port,
                                            s.proto.label(), s.encoding.label()
                                        );
                                        let selected = app.selected_site == Some(*si);
                                        let r = ui
                                            .selectable_label(selected, &label)
                                            .on_hover_text(format!(
                                                "点击选中\nTERM={} 心跳={}s/{}",
                                                s.termtype, s.keepalive_sec, s.keepalive_text
                                            ));
                                        if r.clicked() {
                                            app.selected_site = Some(*si);
                                        }
                                        if ui.small_button("编辑").clicked() {
                                            app.draft_site = s.clone();
                                            app.edit_original_name = Some(s.name.clone());
                                            to_edit = Some(*si);
                                        }
                                        if ui.small_button("连接").clicked() {
                                            to_connect = Some(s.clone());
                                        }
                                    });
                                }
                                if list.is_empty() {
                                    ui.label("（空分组）");
                                }
                            });
                            if g != "未分组" && ui.small_button("✕").on_hover_text("删除此分组（站点移入未分组）").clicked() {
                                del_group = Some(g.clone());
                            }
                        });
                    }
                });
        });

    // 回车直连选中站点
    if open && to_connect.is_none() && !app.show_group_input {
        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            if let Some(si) = app.selected_site {
                if let Some(s) = app.cfg.sites.get(si).cloned() {
                    to_connect = Some(s);
                }
            }
        }
    }

    // 删除分组：把该组站点移入未分组，再从列表移除
    if let Some(g) = del_group {
        for s in app.cfg.sites.iter_mut() {
            if s.group == g {
                s.group = String::new();
            }
        }
        app.cfg.groups.retain(|x| x != &g);
        app.selected_site = None;
        app.save_cfg();
    }

    if !open {
        app.dlg = Dlg::None;
    } else if let Some(i) = to_edit {
        app.dlg = Dlg::Site(i);
    } else if let Some(s) = to_connect {
        // connect_site 可能弹出 SSH 认证框（dlg=SshAuth），此处不能再覆盖关闭
        app.connect_site(s);
    }
}

// ------------------------------------------------------------ 站点编辑

fn site_editor(ctx: &egui::Context, app: &mut App, idx: Option<usize>) {
    let mut open = true;
    let mut ok = false;
    let mut cancel = false;
    let mut del = false;
    let title = if idx.is_some() { "编辑站点" } else { "新建站点" };

    egui::Window::new(title)
        .id(Id::new("dlg_site"))
        .open(&mut open)
        .collapsible(false)
        .default_pos([180.0, 120.0])
        .show(ctx, |ui| {
            let s = &mut app.draft_site;
            egui::Grid::new("sitegrid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("站点名称");
                ui.add(egui::TextEdit::singleline(&mut s.name).desired_width(230.0));
                ui.end_row();

                ui.label("主机地址");
                ui.add(
                    egui::TextEdit::singleline(&mut s.host)
                        .hint_text("bbs.example.com 或 user@1.2.3.4")
                        .desired_width(230.0),
                );
                ui.end_row();

                ui.label("协议");
                ui.horizontal(|ui| {
                    let before = s.proto;
                    ui.selectable_value(&mut s.proto, Protocol::Telnet, "Telnet");
                    ui.selectable_value(&mut s.proto, Protocol::Ssh, "SSH");
                    // 切换协议时把端口末两位改成 23 / 22（保留高位，如 2322↔2323）
                    if s.proto != before {
                        s.port = port_for_proto(s.port, s.proto);
                    }
                    ui.label("端口");
                    ui.add(egui::DragValue::new(&mut s.port).range(1..=65535));
                });
                ui.end_row();

                ui.label("字符编码");
                ui.horizontal(|ui| {
                    for e in [
                        CharEncoding::Gbk,
                        CharEncoding::Big5,
                        CharEncoding::Utf8,
                        CharEncoding::Latin1,
                    ] {
                        ui.selectable_value(&mut s.encoding, e, e.label());
                    }
                });
                ui.end_row();

                ui.label("输入内码转换");
                ui.horizontal(|ui| {
                    for c in [InputConv::None, InputConv::GbToBig5, InputConv::Big5ToGb] {
                        ui.selectable_value(&mut s.input_conv, c, c.label());
                    }
                });
                ui.end_row();

                ui.label("分组");
                ui.horizontal(|ui| {
                    let mut names: Vec<String> = app.cfg.groups.clone();
                    for s in &app.cfg.sites {
                        if !s.group.is_empty() && !names.contains(&s.group) {
                            names.push(s.group.clone());
                        }
                    }
                    let cur = if s.group.is_empty() { "未分组".to_string() } else { s.group.clone() };
                    egui::ComboBox::from_id_salt("site_group")
                        .selected_text(cur)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut s.group, String::new(), "未分组");
                            for n in &names {
                                ui.selectable_value(&mut s.group, n.clone(), n);
                            }
                        });
                });
                ui.end_row();

                ui.label("终端类型 TERM");
                ui.add(egui::TextEdit::singleline(&mut s.termtype).desired_width(120.0));
                ui.end_row();

                ui.label("初始尺寸");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut s.cols).range(20..=400).suffix(" 列"));
                    ui.add(egui::DragValue::new(&mut s.rows).range(6..=200).suffix(" 行"));
                });
                ui.end_row();

                ui.label("SOCKS5 代理");
                ui.horizontal(|ui| {
                    let names: Vec<String> = app
                        .cfg
                        .proxies
                        .iter()
                        .map(|p| format!("{} ({}:{})", p.name, p.host, p.port))
                        .collect();
                    let cur = match s.proxy {
                        Some(i) => names
                            .get(i)
                            .cloned()
                            .unwrap_or_else(|| "已失效，请重选".to_string()),
                        None => "不使用代理".to_string(),
                    };
                    egui::ComboBox::from_id_salt("site_proxy")
                        .selected_text(cur)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut s.proxy, None, "不使用代理");
                            for (i, n) in names.iter().enumerate() {
                                ui.selectable_value(&mut s.proxy, Some(i), n);
                            }
                        });
                    if ui.button("＋ 新建代理").clicked() {
                        app.draft_proxy = Proxy::default();
                    }
                });
                ui.end_row();

                ui.label("防掉线心跳");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut s.keepalive_text)
                            .desired_width(90.0)
                            .hint_text("^@ 或 ^[[B"),
                    );
                    ui.add(
                        egui::DragValue::new(&mut s.keepalive_sec)
                            .range(0..=600)
                            .suffix(" 秒")
                            .speed(1),
                    );
                    ui.label("（0 = 关闭）");
                });
                ui.end_row();

                ui.label("提示音");
                ui.checkbox(&mut s.ring_bell, "收到 BEL 时状态栏提示");
                ui.end_row();
            });

            ui.separator();
            ui.collapsing("自动登录", |ui| {
                let a = &mut s.autologin;
                ui.checkbox(&mut a.enabled, "启用自动登录");
                ui.horizontal(|ui| {
                    ui.label("用户名");
                    ui.add(egui::TextEdit::singleline(&mut a.user).desired_width(200.0));
                });
                ui.horizontal(|ui| {
                    ui.label("密码");
                    ui.add(
                        egui::TextEdit::singleline(&mut a.pass)
                            .password(true)
                            .desired_width(200.0),
                    );
                });
                if s.proto == Protocol::Telnet {
                    ui.horizontal(|ui| {
                        ui.label("账号提示符");
                        ui.add(
                            egui::TextEdit::singleline(&mut a.user_prompt)
                                .hint_text("留空即直接发送")
                                .desired_width(200.0),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("密码提示符");
                        ui.add(
                            egui::TextEdit::singleline(&mut a.pass_prompt)
                                .hint_text("Password / 密码")
                                .desired_width(200.0),
                        );
                    });
                } else {
                    ui.label("SSH 走 password / keyboard-interactive 认证；未填用户名时连接前会弹窗询问")
                        .on_hover_text("服务器提示「请输入密码」时用密码回答");
                }
                // 连接后命令：连接建立几乎立刻发送，早于账号输入
                ui.horizontal(|ui| {
                    ui.label("连接后命令");
                    ui.add(
                        egui::TextEdit::singleline(&mut a.post_connect)
                            .hint_text("如 bbs^M（留空不发）")
                            .desired_width(300.0),
                    );
                });
                ui.label("连接后命令在连接建立后立刻发送，早于自动/手动输入账号")
                    .on_hover_text("可用于进站即切换模式/进入指定界面，如发送 bbs 回车");
                // 登录后命令：Telnet / SSH 通用，登录完成后自动发送
                ui.horizontal(|ui| {
                    ui.label("登录后命令");
                    ui.add(
                        egui::TextEdit::singleline(&mut a.after)
                            .hint_text("如 ^M a b c d ^[[A ^[[B")
                            .desired_width(300.0),
                    );
                });
                ui.label("记法：^M=回车 ^I=Tab ^@=NUL ^[=ESC；^[[A/B/C/D=上/下/右/左；其余字符原样发送")
                    .on_hover_text("登录完成后按此序列自动发送，可用于自动进入版面、翻页等");
                ui.label("延时：~100=等 100 毫秒，~1000=等 1 秒（~~ 表示字面 ~）")
                    .on_hover_text("例如 ^M~500g^M 表示回车后等半秒再按 g 回车");
            });

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("确定保存").clicked() {
                    ok = true;
                }
                if idx.is_some() && ui.button("删除此站点").clicked() {
                    del = true;
                }
                if ui.button("取消").clicked() {
                    cancel = true;
                }
            });
        });

    if ok {
        let draft = app.draft_site.clone();
        // 分组不存在则先创建
        if !draft.group.is_empty() && !app.cfg.groups.contains(&draft.group) {
            app.cfg.groups.push(draft.group.clone());
        }
        // 判断更新还是新增：名称未修改 → 更新原记录；名称修改了 → 新增
        let original = app.edit_original_name.clone().unwrap_or_default();
        if !original.is_empty() && draft.name == original {
            // 名称未变：找到原记录并更新
            if let Some(i) = idx {
                if i < app.cfg.sites.len() {
                    app.cfg.sites[i] = draft;
                }
            } else if let Some(i) = app.cfg.sites.iter().position(|s| s.name == original) {
                app.cfg.sites[i] = draft;
            } else {
                app.cfg.sites.push(draft);
            }
        } else {
            // 名称变了或全新站点：新增
            app.cfg.sites.push(draft);
        }
        app.edit_original_name = None;
        app.save_cfg();
        app.dlg = Dlg::AddressBook;
    } else if del {
        if let Some(i) = idx {
            if i < app.cfg.sites.len() {
                app.cfg.sites.remove(i);
                app.selected_site = None;
                app.save_cfg();
            }
        }
        app.edit_original_name = None;
        app.dlg = Dlg::AddressBook;
    } else if cancel || !open {
        app.edit_original_name = None;
        app.dlg = Dlg::None;
    }
    // 否则保持当前 dlg，窗口持续显示
}

// ------------------------------------------------------------ 代理编辑

fn proxy_editor(ctx: &egui::Context, app: &mut App, idx: Option<usize>) {
    let mut open = true;
    let mut ok = false;
    let mut cancel = false;
    let mut test = false;
    let title = if idx.is_some() { "编辑 SOCKS5 代理" } else { "新建 SOCKS5 代理" };

    egui::Window::new(title)
        .id(Id::new("dlg_proxy"))
        .open(&mut open)
        .collapsible(false)
        .default_pos([240.0, 160.0])
        .show(ctx, |ui| {
            let p = &mut app.draft_proxy;
            egui::Grid::new("proxygrid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("名称");
                ui.add(egui::TextEdit::singleline(&mut p.name).desired_width(200.0));
                ui.end_row();
                ui.label("服务器地址");
                ui.add(
                    egui::TextEdit::singleline(&mut p.host)
                        .hint_text("127.0.0.1")
                        .desired_width(200.0),
                );
                ui.end_row();
                ui.label("端口");
                ui.add(egui::DragValue::new(&mut p.port).range(1..=65535));
                ui.end_row();
                ui.label("认证");
                ui.checkbox(&mut p.auth, "需要用户名/密码");
                ui.end_row();
                if p.auth {
                    ui.label("用户名");
                    ui.add(egui::TextEdit::singleline(&mut p.user).desired_width(200.0));
                    ui.end_row();
                    ui.label("密码");
                    ui.add(
                        egui::TextEdit::singleline(&mut p.pass)
                            .password(true)
                            .desired_width(200.0),
                    );
                    ui.end_row();
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("确定保存").clicked() {
                    ok = true;
                }
                if ui.button("连通性测试").clicked() {
                    test = true;
                }
                if ui.button("取消").clicked() {
                    cancel = true;
                }
            });
            ui.label(
                "说明：代理用于 Telnet 与 SSH 两种连接（SOCKS5 TCP 隧道）；\
                 勾选认证时会用用户名/密码登录代理。",
            );
        });

    if ok {
        match idx {
            Some(i) => app.cfg.proxies[i] = app.draft_proxy.clone(),
            None => app.cfg.proxies.push(app.draft_proxy.clone()),
        }
        app.save_cfg();
        app.dlg = Dlg::Settings;
    } else if cancel || !open {
        app.dlg = Dlg::Settings;
    } else if test {
        let p = app.draft_proxy.clone();
        let msg = match std::net::ToSocketAddrs::to_socket_addrs(&(p.host.as_str(), p.port)) {
            Ok(mut addrs) => match addrs.next() {
                Some(addr) => {
                    match std::net::TcpStream::connect_timeout(
                        &addr,
                        std::time::Duration::from_millis(1500),
                    ) {
                        Ok(_) => format!(
                            "代理 {}:{} TCP 可达（SOCKS5 握手在连接站点时验证）",
                            p.host, p.port
                        ),
                        Err(e) => format!("代理 {}:{} 不可达：{e}", p.host, p.port),
                    }
                }
                None => format!("代理主机 {} 无法解析", p.host),
            },
            Err(e) => format!("代理地址无效：{e}"),
        };
        app.status_line = msg;
        // 保持当前 dlg 不变，窗口继续显示
    }
    // 否则保持当前 dlg，窗口持续显示
}

// ------------------------------------------------------------ SSH 认证

/// SSH 连接前询问用户名/密码（地址不带用户名时弹出）。
fn ssh_auth(ctx: &egui::Context, app: &mut App) {
    let Some(site) = app.ssh_pending.clone() else {
        app.dlg = Dlg::None;
        return;
    };
    let mut open = true;
    let mut ok = false;
    let mut cancel = false;

    egui::Window::new("SSH 认证")
        .id(Id::new("dlg_ssh_auth"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        // 弹窗锚定 ATerm 界面正中
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("ℹ");
                ui.strong(&site.host);
            });
            ui.add_space(4.0);
            egui::Grid::new("sshauthgrid")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    ui.label("用户名");
                    ui.add(
                        egui::TextEdit::singleline(&mut app.ssh_user)
                            .id(Id::new("ssh_auth_user"))
                            .desired_width(200.0),
                    );
                    ui.end_row();
                    ui.label("密码");
                    ui.add(
                        egui::TextEdit::singleline(&mut app.ssh_pass)
                            .id(Id::new("ssh_auth_pass"))
                            .password(true)
                            .desired_width(200.0),
                    );
                    ui.end_row();
                });
            ui.add_space(6.0);
            let user_empty = app.ssh_user.trim().is_empty();
            let pass_empty = app.ssh_pass.is_empty();
            let can_ok = !user_empty && !pass_empty;
            ui.horizontal(|ui| {
                if ui.button("放弃").clicked() {
                    cancel = true;
                }
                if ui.add_enabled(can_ok, egui::Button::new("确认")).clicked() {
                    ok = true;
                }
            });
            // 回车：两栏都填好才连接；否则不连接，聚焦空的输入框等待输入
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if can_ok {
                    ok = true;
                } else if user_empty {
                    ui.memory_mut(|m| m.request_focus(Id::new("ssh_auth_user")));
                } else {
                    ui.memory_mut(|m| m.request_focus(Id::new("ssh_auth_pass")));
                }
            }
        });

    if ok {
        let mut s = site;
        s.autologin.enabled = true;
        s.autologin.user = app.ssh_user.trim().to_string();
        s.autologin.pass = app.ssh_pass.clone();
        app.ssh_pending = None;
        app.ssh_user.clear();
        app.ssh_pass.clear();
        app.dlg = Dlg::None;
        app.connect_site_now(s);
    } else if cancel || !open {
        app.ssh_pending = None;
        app.ssh_user.clear();
        app.ssh_pass.clear();
        app.dlg = Dlg::None;
        app.status_line = "已放弃 SSH 连接".into();
    }
}

// ------------------------------------------------------------ 设置

fn settings(ctx: &egui::Context, app: &mut App) {
    let mut open = true;
    let mut new_proxy = false;
    let mut edit_proxy: Option<usize> = None;

    egui::Window::new("设置")
        .id(Id::new("dlg_settings"))
        .open(&mut open)
        .collapsible(false)
        .default_size([660.0, 520.0])
        // 锚定 ATerm 界面正中
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.group(|ui| {
                        ui.label("显示与外观");
                        ui.label("字号请在标签右键 → 属性中按窗口调整");
                        ui.checkbox(&mut app.cfg.cursor_blink, "光标闪烁");
                        ui.checkbox(&mut app.cfg.copy_on_select, "鼠标划选即复制到剪贴板");
                        ui.checkbox(&mut app.cfg.confirm_quit, "退出时若有活动连接则确认");
                        ui.label("终端键位：Ctrl+Insert=复制  Shift+Insert=粘贴  Ctrl+Shift+R=重连");
                        ui.label("Ctrl+C / X / V 等控制键直接发往 BBS，不作复制粘贴");
                        ui.horizontal(|ui| {
                            ui.label("配色方案");
                            let names: Vec<String> =
                                app.cfg.schemes.iter().map(|s| s.name.clone()).collect();
                            if names.is_empty() {
                                ui.label("(无)");
                            } else {
                                egui::ComboBox::from_id_salt("settings_scheme")
                                    .selected_text(
                                        names[app.cfg.scheme.min(names.len() - 1)].clone(),
                                    )
                                    .show_ui(ui, |ui| {
                                        for (i, n) in names.iter().enumerate() {
                                            ui.selectable_value(&mut app.cfg.scheme, i, n);
                                        }
                                    });
                            }
                        });
                    });

                    ui.group(|ui| {
                        ui.label("SOCKS5 代理列表");
                        let list = app.cfg.proxies.clone();
                        if list.is_empty() {
                            ui.label("（尚未配置代理）");
                        }
                        for (i, p) in list.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(format!(
                                    "{}：{}:{}{}",
                                    p.name,
                                    p.host,
                                    p.port,
                                    if p.auth { "  认证" } else { "" }
                                ));
                                if ui.small_button("编辑").clicked() {
                                    app.draft_proxy = p.clone();
                                    edit_proxy = Some(i);
                                }
                                if ui.small_button("删除").clicked() {
                                    app.cfg.proxies.remove(i);
                                    for s in app.cfg.sites.iter_mut() {
                                        match s.proxy {
                                            Some(x) if x == i => s.proxy = None,
                                            Some(x) if x > i => s.proxy = Some(x - 1),
                                            _ => {}
                                        }
                                    }
                                }
                            });
                        }
                        if ui.button("＋ 添加代理").clicked() {
                            app.draft_proxy = Proxy::default();
                            new_proxy = true;
                        }
                    });

                    ui.group(|ui| {
                        ui.label("按键宏：检测到「触发」按键时，改为发送「发送」序列");
                        ui.label("触发栏不能手输：点击该栏后，按一次键（含组合键）即设定，Esc 取消");
                        ui.label(
                            "发送栏记法：^M=回车  ^I=Tab  ^@=NUL  ^[=ESC  ^[[A/B/C/D=上/下/右/左  ^^=字面^",
                        );
                        ui.label("延时：~100=等 100 毫秒，~1000=等 1 秒（~~ 表示字面 ~）");
                        ui.label("宏仅在终端界面激活时生效，本设置窗口打开期间一律不触发");
                        ui.label("Ctrl+Shift+M 切换全局宏开关（仅本次运行有效，不写入设置文件）");
                        ui.label(
                            "触发键可用 Ctrl+数字：区分大键盘(^!数字)与小键盘(^#数字)，仅作宏触发标识、不发往远端；未绑定或宏停用时按下无效果",
                        );
                        ui.separator();

                        let mut del: Option<usize> = None;
                        let mut arm: Option<usize> = None;
                        let mut disarm = false;
                        let mut add = false;
                        let n = app.cfg.macros.len();
                        egui::ScrollArea::vertical()
                            .id_salt("macro_list")
                            .max_height(220.0)
                            .show(ui, |ui| {
                                for i in 0..n {
                                    // 先取快照，避免与 macros 的可变借用冲突
                                    let armed = app.trigger_armed == Some(i);
                                    let m = &mut app.cfg.macros[i];
                                    ui.horizontal(|ui| {
                                        ui.checkbox(&mut m.enabled, "");
                                        // 触发栏：只读展示，点击后由下一次按键决定内容
                                        let label = if armed {
                                            "请按键…".to_string()
                                        } else if m.trigger.is_empty() {
                                            "点击后按键".to_string()
                                        } else if m.trigger == " " {
                                            // 空格触发键单独标注，避免显示成空白以为没设上
                                            "空格".to_string()
                                        } else {
                                            m.trigger.clone()
                                        };
                                        let btn = egui::Button::new(
                                            egui::RichText::new(label).monospace(),
                                        )
                                        .min_size(egui::vec2(120.0, 0.0))
                                        .fill(if armed {
                                            egui::Color32::from_rgb(70, 60, 20)
                                        } else {
                                            ui.visuals().extreme_bg_color
                                        });
                                        if ui.add(btn).on_hover_text("点击后按一次键设定触发键").clicked() {
                                            if armed {
                                                disarm = true;
                                            } else {
                                                arm = Some(i);
                                            }
                                        }
                                        ui.label("→");
                                        ui.add(
                                            egui::TextEdit::singleline(&mut m.keys)
                                                .desired_width(180.0)
                                                .hint_text("发送，如 ^M^M"),
                                        );
                                        ui.add(
                                            egui::TextEdit::singleline(&mut m.name)
                                                .desired_width(90.0)
                                                .hint_text("备注"),
                                        );
                                        if ui.small_button("清").on_hover_text("清空触发键").clicked() {
                                            m.trigger.clear();
                                        }
                                        if ui.small_button("删").clicked() {
                                            del = Some(i);
                                        }
                                    });
                                }
                            });
                        if disarm {
                            app.trigger_armed = None;
                        }
                        if let Some(i) = arm {
                            app.trigger_armed = Some(i);
                        }
                        if let Some(i) = del {
                            if app.trigger_armed == Some(i) {
                                app.trigger_armed = None;
                            }
                            app.cfg.macros.remove(i);
                            app.save_cfg();
                        }
                        ui.horizontal(|ui| {
                            if ui.button("＋ 新增宏").clicked() {
                                add = true;
                            }
                            ui.label("勾选左侧复选框才生效；触发键留空的条目不生效");
                        });
                        if add {
                            app.cfg.macros.push(Macro {
                                name: String::new(),
                                trigger: String::new(),
                                keys: String::new(),
                                enabled: true,
                            });
                            app.save_cfg();
                        }
                    });

                    ui.group(|ui| {
                        ui.label("配置与数据文件");
                        ui.label(app.cfg_path_shown.clone());
                        ui.horizontal(|ui| {
                            if ui.button("立即保存").clicked() {
                                app.save_cfg();
                            }
                            if ui.button("打开配置目录").clicked() {
                                if let Some(dir) = config::config_path().parent() {
                                    let _ = open_in_explorer(&dir.display().to_string());
                                }
                            }
                            if ui.button("恢复默认站点与宏").clicked() {
                                let d = config::Settings::default();
                                app.cfg.sites = d.sites;
                                app.cfg.groups = d.groups;
                                app.cfg.macros = d.macros;
                                app.save_cfg();
                            }
                        });
                    });
                });
        });

    if !open {
        app.dlg = Dlg::None;
        app.trigger_armed = None;
        app.save_cfg();
    } else if new_proxy {
        app.dlg = Dlg::ProxyNew;
    } else if let Some(i) = edit_proxy {
        app.dlg = Dlg::Proxy(i);
    }
}

fn open_in_explorer(path: &str) -> std::io::Result<()> {
    std::process::Command::new("explorer").arg(path).spawn()?;
    Ok(())
}

/// 切换协议时换算端口：把末两位改成 23（Telnet）/ 22（SSH），保留高位。
/// 例：2322 --Telnet--> 2323；2323 --SSH--> 2322；80 --Telnet--> 23。
/// 端口不足两位（<100）时直接用协议默认端口。
pub fn port_for_proto(cur: u16, proto: Protocol) -> u16 {
    let tail = if proto == Protocol::Telnet { 23u16 } else { 22 };
    if cur < 100 {
        return tail;
    }
    let high = cur / 100 * 100;
    let p = high + tail;
    if p == 0 { tail } else { p }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_switch_keeps_high_digits() {
        // 枫林在线这类高位端口：只改末两位
        assert_eq!(port_for_proto(2322, Protocol::Telnet), 2323);
        assert_eq!(port_for_proto(2323, Protocol::Ssh), 2322);
        assert_eq!(port_for_proto(2322, Protocol::Ssh), 2322);
        assert_eq!(port_for_proto(1234, Protocol::Telnet), 1223);
        assert_eq!(port_for_proto(1234, Protocol::Ssh), 1222);
        // 标准端口
        assert_eq!(port_for_proto(22, Protocol::Telnet), 23);
        assert_eq!(port_for_proto(23, Protocol::Ssh), 22);
        assert_eq!(port_for_proto(80, Protocol::Telnet), 23);
        // 边界：整百端口与最大端口
        assert_eq!(port_for_proto(100, Protocol::Telnet), 123);
        assert_eq!(port_for_proto(65500, Protocol::Ssh), 65522);
    }
}
