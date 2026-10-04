mod advisor;
mod api;
mod datapack;
mod gsi;
mod models;
mod overlay;
mod vision;

use eframe::egui;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const GSI_CONFIG: &str = include_str!("../gamestate_integration_lanetheory.cfg");

fn steam_roots() -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from(r"C:\Program Files (x86)\Steam"),
        PathBuf::from(r"C:\Program Files\Steam"),
    ];
    if let Ok(output) = std::process::Command::new("reg")
        .args(["query", r"HKCU\Software\Valve\Steam", "/v", "SteamPath"])
        .output()
    {
        if let Ok(text) = String::from_utf8(output.stdout) {
            if let Some(path) = text.split_whitespace().last() {
                roots.push(PathBuf::from(path));
            }
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

fn discover_dota_cfg_dir() -> Option<PathBuf> {
    let mut libraries = steam_roots();
    for steam in steam_roots() {
        let vdf = steam.join("steamapps").join("libraryfolders.vdf");
        if let Ok(contents) = std::fs::read_to_string(vdf) {
            for line in contents.lines().filter(|line| line.contains("\"path\"")) {
                let fields: Vec<_> = line.split('"').collect();
                if let Some(path) = fields.get(3) {
                    libraries.push(PathBuf::from(path.replace(r"\\", r"\")));
                }
            }
        }
    }

    libraries.into_iter().find_map(|library| {
        let cfg = library.join("steamapps").join("common").join("dota 2 beta").join("game").join("dota").join("cfg");
        if cfg.parent().is_some_and(|dota| dota.is_dir()) {
            Some(cfg)
        } else {
            None
        }
    })
}

fn install_gsi_config() {
    let Some(cfg_dir) = discover_dota_cfg_dir() else {
        eprintln!("Dota 2 не найдена: GSI cfg будет создан при следующем запуске после установки игры");
        return;
    };
    let target = cfg_dir.join("gamestate_integration_lanetheory.cfg");
    let existing = std::fs::read_to_string(&target).ok();
    if existing.as_deref() == Some(GSI_CONFIG) {
        println!("GSI cfg проверен: {}", target.display());
        return;
    }
    if existing.as_deref().is_some_and(|contents| !contents.contains("LaneTheory Game State Integration Configuration")) {
        eprintln!("GSI cfg с таким именем уже принадлежит другому приложению: {}. Не перезаписываю; проверь uri http://127.0.0.1:3000/", target.display());
        return;
    }
    match std::fs::create_dir_all(&cfg_dir).and_then(|_| std::fs::write(&target, GSI_CONFIG)) {
        Ok(()) => println!("GSI cfg установлен/обновлён: {}. В Steam launch options добавь -gamestateintegration и полностью перезапусти Dota 2.", target.display()),
        Err(error) => eprintln!("Не удалось установить GSI cfg в {}: {error}", target.display()),
    }
}

fn main() -> eframe::Result<()> {
    install_gsi_config();
    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    let handle = rt.handle().clone();

    let (api, state) = rt.block_on(async {
        let api = api::DotaApiClient::new().await;
        let state = models::LiveGameState::default();
        (api, state)
    });

    let shared_state = Arc::new(Mutex::new(state));
    let gsi_hero_names = Arc::new(
        api.heroes
            .iter()
            .map(|(id, hero)| (*id, hero.name.clone()))
            .collect(),
    );
    let shared_api = Arc::new(tokio::sync::Mutex::new(api));

    // Keep the tokio runtime context active on the main thread
    let _guard = rt.enter();
    gsi::GsiServer::start(shared_state.clone(), shared_api.clone(), gsi_hero_names);

    let (screen_w, screen_h) = unsafe {
        let w = GetSystemMetrics(0); // SM_CXSCREEN
        let h = GetSystemMetrics(1); // SM_CYSCREEN
        (
            if w > 0 { w as f32 } else { 1920.0 },
            if h > 0 { h as f32 } else { 1080.0 },
        )
    };

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("LaneTheory")
            .with_transparent(true)
            .with_decorations(false)
            .with_always_on_top()
            .with_mouse_passthrough(true)
            .with_inner_size(egui::vec2(screen_w, screen_h))
            .with_position(egui::pos2(0.0, 0.0)),
        ..Default::default()
    };

    eframe::run_native(
        "LaneTheory",
        options,
        Box::new(move |cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);

            let mut fonts = egui::FontDefinitions::default();
            if let Ok(font_data) = std::fs::read("C:\\Windows\\Fonts\\segoeui.ttf") {
                fonts.font_data.insert(
                    "segoe_ui".to_owned(),
                    std::sync::Arc::new(egui::FontData::from_owned(font_data)),
                );
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, "segoe_ui".to_owned());
            }
            cc.egui_ctx.set_fonts(fonts);

            Ok(Box::new(overlay::OverlayApp::new(shared_state, shared_api, handle)))
        }),
    )
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetSystemMetrics(n_index: i32) -> i32;
}
