//! 系统托盘：关闭窗口后程序留在后台，下载继续。

use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

pub enum Action {
    None,
    Show,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    show_id: MenuId,
    quit_id: MenuId,
    tooltip: String,
}

impl Tray {
    pub fn new() -> Option<Tray> {
        let menu = Menu::new();
        let show = MenuItem::new("显示主窗口", true, None);
        let quit = MenuItem::new("退出 DLKit", true, None);
        menu.append_items(&[
            &show,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .ok()?;

        let mut builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("DLKit · 空闲");
        if let Some(icon) = make_icon() {
            builder = builder.with_icon(icon);
        }
        let icon = builder.build().ok()?;

        Some(Tray {
            icon,
            show_id: show.id().clone(),
            quit_id: quit.id().clone(),
            tooltip: "DLKit · 空闲".into(),
        })
    }

    /// 取出待处理的托盘事件（点击图标 / 菜单项）
    pub fn poll(&self) -> Action {
        let mut action = Action::None;
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            if ev.id == self.show_id {
                action = Action::Show;
            } else if ev.id == self.quit_id {
                action = Action::Quit;
            }
        }
        while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = ev
            {
                action = Action::Show;
            }
        }
        action
    }

    /// 更新悬停提示（下载速度 / 任务数）
    pub fn set_tooltip(&mut self, text: String) {
        if text == self.tooltip {
            return;
        }
        self.tooltip = text;
        let _ = self.icon.set_tooltip(Some(self.tooltip.clone()));
    }
}

/// 程序化绘制 32×32 图标：天蓝圆底 + 白色下箭头
fn make_icon() -> Option<Icon> {
    const S: u32 = 32;
    let mut rgba = vec![0u8; (S * S * 4) as usize];
    let c = (S as f32 - 1.0) / 2.0;
    let radius = c - 0.5;
    for y in 0..S {
        for x in 0..S {
            let fx = x as f32;
            let fy = y as f32;
            let d = ((fx - c).powi(2) + (fy - c).powi(2)).sqrt();
            let alpha = ((radius + 0.5 - d).clamp(0.0, 1.0) * 255.0) as u8;
            if alpha == 0 {
                continue;
            }
            let stem = (13.5..=18.5).contains(&fx) && (7.0..=17.0).contains(&fy);
            let head = (15.5..=25.0).contains(&fy) && (fx - c).abs() <= (25.0 - fy) * 0.95;
            let (r, g, b) = if stem || head {
                (255, 255, 255)
            } else {
                (0x4b, 0x9a, 0xc4)
            };
            let i = ((y * S + x) * 4) as usize;
            rgba[i] = r;
            rgba[i + 1] = g;
            rgba[i + 2] = b;
            rgba[i + 3] = alpha;
        }
    }
    Icon::from_rgba(rgba, S, S).ok()
}
