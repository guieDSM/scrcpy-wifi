#![windows_subsystem = "windows"]

mod adb;
mod install;

use adb::{Adb, Device, Service, ServiceKind};
use eframe::egui::{self, Color32, RichText};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const GREEN: Color32 = Color32::from_rgb(52, 168, 83);
const AMBER: Color32 = Color32::from_rgb(230, 162, 0);
const RED: Color32 = Color32::from_rgb(217, 48, 37);
const GREY: Color32 = Color32::from_rgb(140, 140, 140);
const ACCENT: Color32 = Color32::from_rgb(26, 115, 232);

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("scrcpy Wi-Fi")
            .with_inner_size([440.0, 620.0])
            .with_min_inner_size([380.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native("scrcpy-wifi", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

// ---------- settings ----------

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    quality: u8,
    audio: bool,
    screen_off: bool,
    stay_awake: bool,
    on_top: bool,
    auto_open: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { quality: 1, audio: true, screen_off: false, stay_awake: true, on_top: false, auto_open: true }
    }
}

impl Settings {
    fn args(&self) -> Vec<String> {
        let mut a: Vec<String> = match self.quality {
            0 => vec!["--video-bit-rate=4M", "--max-size=1280", "--max-fps=60"],
            2 => vec!["--video-bit-rate=16M", "--max-fps=60"],
            _ => vec!["--video-bit-rate=8M", "--max-size=1920", "--max-fps=60"],
        }
        .into_iter()
        .map(String::from)
        .collect();
        if !self.audio {
            a.push("--no-audio".into());
        }
        if self.screen_off {
            a.push("--turn-screen-off".into());
        }
        if self.stay_awake {
            a.push("--stay-awake".into());
        }
        if self.on_top {
            a.push("--always-on-top".into());
        }
        a
    }
}

const QUALITY: [&str; 3] = [
    "Плавно — слабый Wi-Fi (1280, 4 Мбит/с)",
    "Баланс (1920, 8 Мбит/с)",
    "Максимум (как на телефоне, 16 Мбит/с)",
];

// ---------- state shared with worker threads ----------

#[derive(Clone, Default)]
struct Shared {
    bin: Option<PathBuf>,
    installing: bool,
    install_error: Option<String>,
    services: Vec<Service>,
    devices: Vec<Device>,
    busy: Option<String>,
    need_pair: bool,
    error: Option<String>,
    log: Vec<String>,
}

impl Shared {
    fn push_log(&mut self, text: impl AsRef<str>) {
        for line in text.as_ref().lines().filter(|l| !l.trim().is_empty()) {
            self.log.push(line.to_string());
        }
        let n = self.log.len();
        if n > 400 {
            self.log.drain(..n - 400);
        }
    }
}

type SharedRef = Arc<Mutex<Shared>>;

fn log(sh: &SharedRef, text: impl AsRef<str>) {
    sh.lock().unwrap().push_log(text);
}

fn wait_until(secs: u64, mut probe: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if probe() {
            return true;
        }
        thread::sleep(Duration::from_millis(700));
    }
    false
}

fn any_online(adb: &Adb) -> bool {
    adb.devices().iter().any(|d| d.online() && d.is_wifi())
}

fn host_of(addr: &str) -> &str {
    addr.rsplit_once(':').map_or(addr, |(h, _)| h)
}

fn connect_task(adb: &Adb, sh: &SharedRef, addr: &str) {
    log(sh, format!("adb connect {addr}"));
    let out = adb.connect(addr);
    log(sh, out);
    let ok = wait_until(5, || any_online(adb));
    let mut g = sh.lock().unwrap();
    if ok {
        g.need_pair = false;
    } else {
        g.need_pair = true;
        g.error = Some(
            "Не удалось подключиться. Если этот компьютер ещё не сопряжён с телефоном — сопрягите его ниже (один раз)."
                .into(),
        );
    }
}

fn pair_task(adb: &Adb, sh: &SharedRef, addr: &str, code: &str) {
    log(sh, format!("adb pair {addr} ******"));
    let (paired, out) = adb.pair(addr, code);
    log(sh, out);
    if !paired {
        sh.lock().unwrap().error =
            Some("Сопряжение не прошло. Проверьте код; окно с кодом на телефоне должно оставаться открытым.".into());
        return;
    }
    {
        let mut g = sh.lock().unwrap();
        g.need_pair = false;
        g.push_log("Сопряжено");
    }
    let host = host_of(addr).to_string();
    let connected = wait_until(10, || {
        if any_online(adb) {
            return true;
        }
        if let Some(s) = adb
            .mdns_services()
            .into_iter()
            .find(|s| s.kind == ServiceKind::Connect && host_of(&s.addr) == host)
        {
            adb.connect(&s.addr);
        }
        false
    });
    if !connected {
        sh.lock().unwrap().error = Some("Сопряжено, но подключиться пока не удалось. Нажмите «Подключить».".into());
    }
}

fn pipe_to_log<R: Read + Send + 'static>(reader: R, sh: SharedRef) {
    thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            log(&sh, line);
        }
    });
}

/// Same phone can be listed twice (ip:port and mDNS name); key it by address.
fn device_addr(d: &Device, services: &[Service]) -> String {
    if let Some(name) = d.mdns_name() {
        if let Some(s) = services.iter().find(|s| s.kind == ServiceKind::Connect && s.name == name) {
            return s.addr.clone();
        }
    }
    d.serial.clone()
}

// ---------- app ----------

struct App {
    ctx: egui::Context,
    shared: SharedRef,
    stop: Arc<AtomicBool>,
    settings: Settings,
    scrcpy: Arc<Mutex<Option<Child>>>,
    selected: Option<String>,
    code: String,
    pair_addr: String,
    manual_addr: String,
    show_pair: bool,
    auto_tried: Vec<String>,
    auto_opened: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_style(&cc.egui_ctx);
        let settings = cc.storage.and_then(|s| eframe::get_value(s, "settings")).unwrap_or_default();
        let app = Self {
            ctx: cc.egui_ctx.clone(),
            shared: Arc::new(Mutex::new(Shared::default())),
            stop: Arc::new(AtomicBool::new(false)),
            settings,
            scrcpy: Arc::new(Mutex::new(None)),
            selected: None,
            code: String::new(),
            pair_addr: String::new(),
            manual_addr: String::new(),
            show_pair: false,
            auto_tried: Vec::new(),
            auto_opened: false,
        };
        app.start_backend();
        app
    }

    /// Installs scrcpy if needed, starts adb, then polls mDNS and devices.
    fn start_backend(&self) {
        let sh = self.shared.clone();
        let stop = self.stop.clone();
        let ctx = self.ctx.clone();
        {
            let mut g = sh.lock().unwrap();
            g.install_error = None;
            g.installing = !install::is_installed();
        }
        thread::spawn(move || {
            let dir = if install::is_installed() {
                install::scrcpy_dir()
            } else {
                let (sh2, ctx2) = (sh.clone(), ctx.clone());
                match install::install(move |m| {
                    log(&sh2, m);
                    ctx2.request_repaint();
                }) {
                    Ok(d) => d,
                    Err(e) => {
                        let mut g = sh.lock().unwrap();
                        g.installing = false;
                        g.push_log(&e);
                        g.install_error = Some(e);
                        ctx.request_repaint();
                        return;
                    }
                }
            };
            {
                let mut g = sh.lock().unwrap();
                g.installing = false;
                g.bin = Some(dir.clone());
            }
            let adb = Adb::new(&dir);
            let (_, out) = adb.run(&["start-server"]);
            log(&sh, out);
            while !stop.load(Ordering::Relaxed) {
                let services = adb.mdns_services();
                let devices = adb.devices();
                {
                    let mut g = sh.lock().unwrap();
                    g.services = services;
                    g.devices = devices;
                }
                ctx.request_repaint();
                for _ in 0..15 {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        });
    }

    fn spawn_task(&self, label: &str, task: impl FnOnce(&Adb, &SharedRef) + Send + 'static) {
        let sh = self.shared.clone();
        let bin = {
            let mut g = sh.lock().unwrap();
            let Some(bin) = g.bin.clone() else { return };
            if g.busy.is_some() {
                return;
            }
            g.busy = Some(label.to_string());
            g.error = None;
            bin
        };
        let ctx = self.ctx.clone();
        thread::spawn(move || {
            let adb = Adb::new(&bin);
            task(&adb, &sh);
            sh.lock().unwrap().busy = None;
            ctx.request_repaint();
        });
    }

    fn connect(&self, addr: String) {
        self.spawn_task("Подключаюсь…", move |adb, sh| connect_task(adb, sh, &addr));
    }

    fn pair(&self, addr: String, code: String) {
        self.spawn_task("Сопрягаю…", move |adb, sh| pair_task(adb, sh, &addr, &code));
    }

    fn open_screen(&mut self, serial: &str, bin: &Path) {
        let args = self.settings.args();
        log(&self.shared, format!("scrcpy -s {serial} {}", args.join(" ")));
        let mut cmd = Command::new(bin.join("scrcpy.exe"));
        cmd.arg("-s")
            .arg(serial)
            .args(&args)
            .current_dir(bin)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        adb::hidden(&mut cmd);
        match cmd.spawn() {
            Ok(mut child) => {
                if let Some(out) = child.stdout.take() {
                    pipe_to_log(out, self.shared.clone());
                }
                if let Some(err) = child.stderr.take() {
                    pipe_to_log(err, self.shared.clone());
                }
                *self.scrcpy.lock().unwrap() = Some(child);
            }
            Err(e) => self.shared.lock().unwrap().error = Some(format!("Не удалось запустить scrcpy: {e}")),
        }
    }

    fn close_screen(&self) {
        if let Some(mut child) = self.scrcpy.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn poll_scrcpy(&self) {
        let finished = {
            let mut g = self.scrcpy.lock().unwrap();
            let state = g.as_mut().map(|c| c.try_wait());
            match state {
                Some(Ok(Some(status))) => {
                    *g = None;
                    Some(status.success())
                }
                Some(Err(_)) => {
                    *g = None;
                    Some(false)
                }
                _ => None,
            }
        };
        if let Some(ok) = finished {
            let mut s = self.shared.lock().unwrap();
            s.push_log(if ok { "Экран закрыт" } else { "scrcpy завершился с ошибкой" });
            if !ok {
                s.error = Some("Экран закрылся с ошибкой — подробности в «Журнале». Чаще всего это обрыв Wi-Fi.".into());
            }
        }
    }

    /// Auto-connect to a phone that showed up and open its screen once per session.
    fn automation(&mut self, snap: &Shared, device: Option<&Device>, running: bool) {
        let Some(bin) = snap.bin.as_ref() else { return };
        if snap.busy.is_some() {
            return;
        }
        if let Some(d) = device {
            if self.settings.auto_open && !running && !self.auto_opened {
                self.auto_opened = true;
                let serial = d.serial.clone();
                let bin = bin.clone();
                self.open_screen(&serial, &bin);
            }
            return;
        }
        if snap.need_pair {
            return;
        }
        if let Some(s) = snap.services.iter().find(|s| s.kind == ServiceKind::Connect) {
            if !self.auto_tried.contains(&s.addr) {
                self.auto_tried.push(s.addr.clone());
                self.connect(s.addr.clone());
            }
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let running = self.scrcpy.lock().map(|g| g.is_some()).unwrap_or(false);
        if !running {
            if let Some(bin) = self.shared.lock().ok().and_then(|g| g.bin.clone()) {
                let _ = Adb::new(&bin).run(&["kill-server"]);
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_scrcpy();
        let snap = self.shared.lock().unwrap().clone();

        let mut online: Vec<(String, Device)> = Vec::new();
        for d in snap.devices.iter().filter(|d| d.online() && d.is_wifi()) {
            let addr = device_addr(d, &snap.services);
            if !online.iter().any(|(a, _)| *a == addr) {
                online.push((addr, d.clone()));
            }
        }
        if self.selected.as_ref().is_none_or(|s| !online.iter().any(|(_, d)| &d.serial == s)) {
            self.selected = online.first().map(|(_, d)| d.serial.clone());
        }
        let device = online.iter().find(|(_, d)| Some(&d.serial) == self.selected.as_ref()).cloned();
        let running = self.scrcpy.lock().unwrap().is_some();
        self.automation(&snap, device.as_ref().map(|(_, d)| d), running);

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(4.0);
                ui.label(RichText::new("scrcpy Wi-Fi").size(24.0).strong());
                ui.label(RichText::new("Экран телефона на компьютере без кабеля").color(GREY));
                ui.add_space(10.0);

                card(ui, |ui| self.ui_status(ui, &snap, &online, device.as_ref(), running));

                if let Some(err) = &snap.error {
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(err).color(RED));
                        if ui.small_button("✕").clicked() {
                            self.shared.lock().unwrap().error = None;
                        }
                    });
                }

                if device.is_none() && snap.bin.is_some() && (snap.need_pair || self.show_pair) {
                    ui.add_space(10.0);
                    card(ui, |ui| self.ui_pairing(ui, &snap));
                }

                ui.add_space(10.0);
                egui::CollapsingHeader::new("Настройки экрана").show(ui, |ui| self.ui_settings(ui));
                egui::CollapsingHeader::new("Как включить отладку по Wi-Fi").show(ui, ui_help);
                egui::CollapsingHeader::new("Журнал").show(ui, |ui| {
                    if ui.small_button("Скопировать").clicked() {
                        ctx.copy_text(snap.log.join("\n"));
                    }
                    egui::ScrollArea::vertical().max_height(180.0).stick_to_bottom(true).show(ui, |ui| {
                        for line in &snap.log {
                            ui.label(RichText::new(line).monospace().size(11.0));
                        }
                    });
                });

                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!("scrcpy {} · © Genymobile, Apache 2.0", install::VERSION))
                        .size(11.0)
                        .color(GREY),
                );
            });
        });

        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "settings", &self.settings);
    }
}

// ---------- UI pieces ----------

impl App {
    fn ui_status(
        &mut self,
        ui: &mut egui::Ui,
        snap: &Shared,
        online: &[(String, Device)],
        device: Option<&(String, Device)>,
        running: bool,
    ) {
        if let Some(err) = &snap.install_error {
            status_line(ui, RED, "Не удалось подготовить scrcpy", false);
            ui.label(err);
            ui.label("Проверьте интернет и повторите.");
            if big_button(ui, "Повторить", true) {
                self.start_backend();
            }
            return;
        }
        let Some(bin) = snap.bin.clone() else {
            status_line(ui, GREY, "Подготовка…", true);
            if snap.installing {
                ui.label("Первый запуск: скачиваю scrcpy (~11 МБ) с официального GitHub.");
            }
            return;
        };

        if let Some((addr, d)) = device {
            let title = if running { "Экран открыт" } else { "Подключено" };
            status_line(ui, GREEN, title, false);
            ui.label(format!("{} · {}", d.model, host_of(addr)));
            if online.len() > 1 {
                egui::ComboBox::from_label("телефон")
                    .selected_text(format!("{} · {}", d.model, host_of(addr)))
                    .show_ui(ui, |ui| {
                        for (a, dev) in online {
                            ui.selectable_value(
                                &mut self.selected,
                                Some(dev.serial.clone()),
                                format!("{} · {}", dev.model, host_of(a)),
                            );
                        }
                    });
            }
            ui.add_space(6.0);
            if running {
                if big_button(ui, "Закрыть экран", true) {
                    self.close_screen();
                }
            } else if big_button(ui, "Открыть экран", true) {
                let serial = d.serial.clone();
                self.open_screen(&serial, &bin);
            }
            return;
        }

        if let Some(busy) = &snap.busy {
            status_line(ui, AMBER, busy, true);
            return;
        }

        if let Some(s) = snap.services.iter().find(|s| s.kind == ServiceKind::Connect) {
            status_line(ui, AMBER, "Телефон найден", false);
            ui.label(format!("Адрес {}", s.addr));
            ui.add_space(6.0);
            if big_button(ui, "Подключить", true) {
                self.connect(s.addr.clone());
            }
        } else {
            status_line(ui, GREY, "Ищу телефон в сети…", true);
            ui.label("Включите на телефоне «Отладку по Wi-Fi». Телефон и компьютер должны быть в одной сети, экран телефона — включён.");
            ui.add_space(4.0);
            egui::CollapsingHeader::new("Ввести адрес вручную").show(ui, |ui| {
                ui.label(RichText::new("«IP-адрес и порт» с экрана «Отладка по Wi-Fi»").color(GREY));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.manual_addr).hint_text("192.168.1.20:41234").desired_width(180.0));
                    let addr = self.manual_addr.trim().to_string();
                    if ui.add_enabled(addr.contains(':'), egui::Button::new("Подключить")).clicked() {
                        self.connect(addr);
                    }
                });
            });
        }

        if !snap.need_pair && !self.show_pair {
            ui.add_space(4.0);
            if ui.link("Новый телефон? Сопрячь").clicked() {
                self.show_pair = true;
            }
        }
    }

    fn ui_pairing(&mut self, ui: &mut egui::Ui, snap: &Shared) {
        ui.label(RichText::new("Сопряжение").size(17.0).strong());
        ui.label(RichText::new("Нужно один раз для каждого компьютера.").color(GREY));
        ui.add_space(4.0);
        ui.label("1. На телефоне: «Отладка по Wi-Fi» → «Подключение с помощью кода подключения». Окно с кодом не закрывайте.");

        let found = snap.services.iter().find(|s| s.kind == ServiceKind::Pairing).map(|s| s.addr.clone());
        let addr = match &found {
            Some(a) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("✔").color(GREEN));
                    ui.label(format!("Окно с кодом найдено ({a})"));
                });
                a.clone()
            }
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Жду окно с кодом на телефоне…");
                });
                ui.label(RichText::new("Не находится? Введите «IP-адрес и порт» из окна с кодом:").color(GREY));
                ui.add(egui::TextEdit::singleline(&mut self.pair_addr).hint_text("192.168.1.20:37000").desired_width(180.0));
                self.pair_addr.trim().to_string()
            }
        };

        ui.add_space(4.0);
        ui.label("2. Введите 6 цифр из окна:");
        let mut submit = false;
        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.code)
                    .hint_text("123456")
                    .font(egui::FontId::monospace(20.0))
                    .desired_width(120.0),
            );
            self.code.retain(|c| c.is_ascii_digit());
            self.code.truncate(6);
            let ready = self.code.len() == 6 && addr.contains(':') && snap.busy.is_none();
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && ready {
                submit = true;
            }
            if ui.add_enabled(ready, egui::Button::new("Сопрячь")).clicked() {
                submit = true;
            }
        });
        if submit {
            let code = std::mem::take(&mut self.code);
            self.pair(addr, code);
        }
        if self.show_pair && !snap.need_pair && ui.link("Отмена").clicked() {
            self.show_pair = false;
        }
    }

    fn ui_settings(&mut self, ui: &mut egui::Ui) {
        egui::ComboBox::from_label("качество")
            .selected_text(QUALITY[self.settings.quality.min(2) as usize])
            .show_ui(ui, |ui| {
                for (i, q) in QUALITY.iter().enumerate() {
                    ui.selectable_value(&mut self.settings.quality, i as u8, *q);
                }
            });
        ui.checkbox(&mut self.settings.audio, "Звук телефона на компьютере");
        ui.checkbox(&mut self.settings.screen_off, "Гасить экран телефона (картинка остаётся)");
        ui.checkbox(&mut self.settings.stay_awake, "Не давать телефону засыпать");
        ui.checkbox(&mut self.settings.on_top, "Окно поверх остальных");
        ui.checkbox(&mut self.settings.auto_open, "Открывать экран сразу после подключения");
        ui.label(RichText::new("Применяется при следующем открытии экрана.").size(12.0).color(GREY));
    }
}

fn ui_help(ui: &mut egui::Ui) {
    ui.label("1. Настройки → О телефоне → 7 раз нажать «Номер сборки».");
    ui.label("2. Настройки → Система → Для разработчиков → «Отладка по Wi-Fi» → включить.");
    ui.label("3. Подключиться в этом окне. В первый раз — сопряжение по коду.");
    ui.label(RichText::new("На Samsung, Xiaomi и других пункты лежат в другом месте — проще найти поиском в настройках.").color(GREY));
    ui.add_space(4.0);
    ui.label("В окне экрана: правая кнопка мыши — «Назад», средняя — «Домой», Ctrl+V — вставить текст, Alt+F — полный экран.");
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(14.0))
        .rounding(10.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

fn status_line(ui: &mut egui::Ui, color: Color32, title: &str, spinner: bool) {
    ui.horizontal(|ui| {
        if spinner {
            ui.spinner();
        } else {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 6.0, color);
        }
        ui.label(RichText::new(title).size(19.0).strong());
    });
}

fn big_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> bool {
    let button = egui::Button::new(RichText::new(text).size(16.0).strong().color(Color32::WHITE))
        .fill(ACCENT)
        .min_size(egui::vec2(ui.available_width(), 40.0));
    ui.add_enabled(enabled, button).clicked()
}

fn setup_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
        fonts.font_data.insert("segoe".to_owned(), egui::FontData::from_owned(bytes));
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "segoe".to_owned());
    }
    ctx.set_fonts(fonts);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 7.0);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(15.0));
        s.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(15.0));
    });
}
