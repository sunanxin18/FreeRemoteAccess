//! 版本化本地显示消息。像素不乘 density；身份/版本与输入 viewport 完全独立。
use frd_core::{
    DisplayGeometry, DisplayIntent, DisplayOrientationPreference, PixelRect, PixelSize,
    ResolutionMode,
};
use serde::Deserialize;

const JS_MAX: u64 = 9_007_199_254_740_991;
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedRect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}
impl SignedRect {
    fn within_display(self, size: PixelSize) -> Option<PixelRect> {
        let rect = PixelRect {
            x: self.left.try_into().ok()?,
            y: self.top.try_into().ok()?,
            width: self.width,
            height: self.height,
        };
        let (_, end) = rect.checked_bounds()?;
        (end.x <= size.width && end.y <= size.height).then_some(rect)
    }
    fn clipped(self, origin_x: i64, origin_y: i64, size: PixelSize) -> Option<PixelRect> {
        if self.width == 0 || self.height == 0 {
            return None;
        }
        let x = i64::from(self.left).checked_add(origin_x)?;
        let y = i64::from(self.top).checked_add(origin_y)?;
        let right = x
            .checked_add(i64::from(self.width))?
            .min(i64::from(size.width));
        let bottom = y
            .checked_add(i64::from(self.height))?
            .min(i64::from(size.height));
        let x = x.max(0);
        let y = y.max(0);
        if right <= x || bottom <= y {
            return None;
        }
        Some(PixelRect {
            x: x.try_into().ok()?,
            y: y.try_into().ok()?,
            width: (right - x).try_into().ok()?,
            height: (bottom - y).try_into().ok()?,
        })
    }
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RawGeometry {
    pub display_id: u64,
    pub width: u32,
    pub height: u32,
    pub density: f64,
    pub rotation: u32,
    // windowRect 属于当前显示器；drawableRect 相对窗口。外接屏不加主屏原点。
    pub window: SignedRect,
    pub drawable: SignedRect,
    pub work_area: Option<SignedRect>,
    pub fullscreen: bool,
}
impl RawGeometry {
    fn geometry(self) -> Option<DisplayGeometry> {
        if self.display_id > JS_MAX
            || self.rotation > 3
            || !self.density.is_finite()
            || self.density <= 0.0
            || self.density > 16.0
        {
            return None;
        }
        let scale = (self.density * 1000.0).round();
        if scale < 1.0 {
            return None;
        }
        let size = PixelSize::new(self.width, self.height)?;
        self.window.clipped(0, 0, size)?;
        // drawable 必须在窗口内；窗口可部分跨屏，最后与所属显示器相交。
        if self.drawable.left < 0
            || self.drawable.top < 0
            || self.drawable.width == 0
            || self.drawable.height == 0
            || u64::from(self.drawable.left as u32) + u64::from(self.drawable.width)
                > u64::from(self.window.width)
            || u64::from(self.drawable.top as u32) + u64::from(self.drawable.height)
                > u64::from(self.window.height)
        {
            return None;
        }
        let content = self.drawable.clipped(
            i64::from(self.window.left),
            i64::from(self.window.top),
            size,
        )?;
        // 系统工作区不可用时使用可绘制内容的保守子集，并明确标记来源。
        let work = match self.work_area {
            Some(area) => area.within_display(size)?,
            None => content,
        };
        DisplayGeometry::new(
            f64::from(self.width) / self.density,
            f64::from(self.height) / self.density,
            size,
            work,
            content,
            scale as u32,
            self.fullscreen,
        )
    }
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayMessageReason {
    WindowNotReady,
    WindowDestroyed,
    DisplayUnavailable,
    InvalidGeometry,
    InvalidWorkArea,
    Background,
}
impl DisplayMessageReason {
    fn label(self) -> &'static str {
        match self {
            Self::WindowNotReady => "窗口尚未就绪",
            Self::WindowDestroyed => "窗口已销毁",
            Self::DisplayUnavailable => "所属显示器不可用",
            Self::InvalidGeometry => "本地显示几何无效",
            Self::InvalidWorkArea => "系统工作区越界，使用可绘制内容区",
            Self::Background => "应用位于后台",
        }
    }
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Message {
    abi_version: u32,
    stage: u64,
    revision: u64,
    window_id: u64,
    phone: bool,
    geometry: Option<RawGeometry>,
    reason: Option<DisplayMessageReason>,
}
impl Message {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() || bytes.len() > 4096 {
            return None;
        }
        let value: Self = serde_json::from_slice(bytes).ok()?;
        value.valid().then_some(value)
    }
    fn valid(&self) -> bool {
        if self.abi_version != 1
            || self.stage == 0
            || self.revision == 0
            || self.stage > JS_MAX
            || self.revision > JS_MAX
            || self.window_id > JS_MAX
        {
            return false;
        }
        match (self.geometry, self.reason) {
            (Some(raw), None) if self.window_id != 0 => raw.geometry().is_some(),
            (Some(raw), Some(DisplayMessageReason::InvalidWorkArea))
                if self.window_id != 0 && raw.work_area.is_none() =>
            {
                raw.geometry().is_some()
            }
            (None, Some(reason)) => !matches!(reason, DisplayMessageReason::InvalidWorkArea),
            _ => false,
        }
    }
}
pub struct LocalDisplay {
    stage: u64,
    revision: u64,
    window: u64,
    pub geometry: Option<DisplayGeometry>,
    phone: bool,
    pub reason: &'static str,
    pub source: &'static str,
    raw: Option<RawGeometry>,
}
impl Default for LocalDisplay {
    fn default() -> Self {
        Self {
            stage: 0,
            revision: 0,
            window: 0,
            geometry: None,
            phone: false,
            reason: "尚无有效本地显示快照",
            source: "unavailable",
            raw: None,
        }
    }
}
impl LocalDisplay {
    pub fn accept(&mut self, message: Message) {
        if !message.valid() {
            return;
        }
        if message.stage < self.stage
            || message.revision <= self.revision
            || (message.stage == self.stage
                && self.window != 0
                && message.window_id != 0
                && message.window_id != self.window)
        {
            return;
        }
        if message.stage != self.stage {
            self.window = 0;
        }
        self.stage = message.stage;
        self.revision = message.revision;
        if message.window_id != 0 {
            self.window = message.window_id;
        }
        self.phone = message.phone;
        self.raw = message.geometry;
        self.geometry = message.geometry.and_then(RawGeometry::geometry);
        self.reason = message.reason.map_or("", DisplayMessageReason::label);
        self.source = match message.geometry {
            Some(raw) if raw.work_area.is_some() => "system_available_area",
            Some(_) => "drawable_conservative_fallback",
            None => "unavailable",
        };
    }
    pub fn invalidate(&mut self) {
        self.raw = None;
        self.geometry = None;
        self.reason = "本地显示消息队列已满";
        self.source = "unavailable";
    }
    pub(crate) fn active_window(&self) -> Option<(u64, u64)> {
        (self.geometry.is_some() && self.stage != 0 && self.window != 0)
            .then_some((self.stage, self.window))
    }
    pub fn evidence(&self) -> serde_json::Value {
        serde_json::json!({"stage": self.stage, "revision": self.revision, "windowId": self.window,
            "displayId": self.raw.map(|raw| raw.display_id), "rotation": self.raw.map(|raw| raw.rotation),
            "density": self.raw.map(|raw| raw.density),
            "windowRect": self.raw.map(|raw| [i64::from(raw.window.left),i64::from(raw.window.top),i64::from(raw.window.width),i64::from(raw.window.height)]),
            "drawableRect": self.raw.map(|raw| [i64::from(raw.drawable.left),i64::from(raw.drawable.top),i64::from(raw.drawable.width),i64::from(raw.drawable.height)]),
            "source": self.source, "reason": self.reason,
            "geometry": self.geometry.map(|g| serde_json::json!({"width":g.physical_size.width,"height":g.physical_size.height,
                "logicalWidth":g.logical_width,"logicalHeight":g.logical_height,"densityMilli":g.scale_factor_milli,
                "workArea":[g.work_area.x,g.work_area.y,g.work_area.width,g.work_area.height],
                "contentArea":[g.content_area.x,g.content_area.y,g.content_area.width,g.content_area.height]}))})
    }
    pub fn freeze(&self, intent: &mut DisplayIntent) {
        if matches!(
            intent.mode,
            ResolutionMode::Fixed(_) | ResolutionMode::ServerManaged
        ) {
            return;
        }
        intent.geometry = self.geometry;
        intent.orientation = if self.phone && intent.mode == ResolutionMode::NativeDisplay {
            DisplayOrientationPreference::Landscape
        } else {
            DisplayOrientationPreference::Preserve
        };
    }
}

#[cfg(test)]
pub(crate) fn sample_message(
    stage: u64,
    revision: u64,
    window: u64,
    phone: bool,
) -> serde_json::Value {
    serde_json::json!({"abiVersion":1,"stage":stage,"revision":revision,"windowId":window,"phone":phone,"reason":null,
        "geometry":{"displayId":7,"width":1260,"height":2720,"density":3.5,"rotation":0,
            "window":{"left":0,"top":0,"width":1260,"height":2720},
            "drawable":{"left":0,"top":100,"width":1260,"height":2520},"workArea":null,"fullscreen":false}})
}
#[cfg(test)]
mod tests {
    use super::*;
    fn parse(value: &serde_json::Value) -> Option<Message> {
        Message::parse(&serde_json::to_vec(value).unwrap())
    }
    fn raw(value: &serde_json::Value) -> DisplayGeometry {
        parse(value).unwrap().geometry.unwrap().geometry().unwrap()
    }
    #[test]
    fn local_display_physical_density_rotation_and_conservative_work_area() {
        let mut value = sample_message(1, 1, 11, true);
        let portrait = raw(&value);
        assert_eq!(portrait.physical_size, PixelSize::new(1260, 2720).unwrap());
        assert_eq!(
            (portrait.logical_width, portrait.scale_factor_milli),
            (360.0, 3500)
        );
        assert_eq!(portrait.work_area, portrait.content_area);
        assert_eq!(portrait.content_area.y, 100);
        value["geometry"]["width"] = 2720.into();
        value["geometry"]["height"] = 1260.into();
        value["geometry"]["rotation"] = 1.into();
        value["geometry"]["window"]["width"] = 2720.into();
        value["geometry"]["window"]["height"] = 1260.into();
        value["geometry"]["drawable"]["width"] = 2720.into();
        value["geometry"]["drawable"]["height"] = 1060.into();
        assert_eq!(
            raw(&value).physical_size,
            PixelSize::new(2720, 1260).unwrap()
        );
    }
    #[test]
    fn local_display_pc_4k_external_screen_local_coordinates_and_signed_window_clip() {
        let mut value = sample_message(1, 1, 11, false);
        value["geometry"]["width"] = 3840.into();
        value["geometry"]["height"] = 2160.into();
        value["geometry"]["density"] = 1.5.into();
        value["geometry"]["window"] =
            serde_json::json!({"left":-50,"top":40,"width":3000,"height":1800});
        value["geometry"]["drawable"] =
            serde_json::json!({"left":10,"top":60,"width":2980,"height":1720});
        value["geometry"]["workArea"] =
            serde_json::json!({"left":0,"top":30,"width":3840,"height":2100});
        let g = raw(&value);
        assert_eq!(g.logical_width, 2560.0);
        assert_eq!(g.physical_size.width, 3840);
        assert_eq!(
            g.content_area,
            PixelRect {
                x: 0,
                y: 100,
                width: 2940,
                height: 1720
            }
        );
        assert_eq!(g.work_area.y, 30);
        // 系统 windowRect 已以所属外接屏为原点；主屏全局偏移不进入消息。
        assert_eq!(parse(&value).unwrap().geometry.unwrap().display_id, 7);
    }
    #[test]
    fn local_display_rejects_invalid_version_units_bounds_identity_and_fields() {
        let base = sample_message(1, 1, 11, true);
        for (path, bad) in [
            (vec!["abiVersion"], serde_json::json!(2)),
            (vec!["stage"], serde_json::json!(0)),
            (vec!["revision"], serde_json::json!(9007199254740992u64)),
            (vec!["windowId"], serde_json::json!(0)),
            (vec!["geometry", "density"], serde_json::json!(0)),
            (vec!["geometry", "density"], serde_json::json!(-1)),
            (vec!["geometry", "density"], serde_json::Value::Null),
            (vec!["geometry", "density"], serde_json::json!(0.00001)),
            (vec!["geometry", "rotation"], serde_json::json!(4)),
            (vec!["geometry", "width"], serde_json::json!(0)),
            (vec!["geometry", "width"], serde_json::json!(1.5)),
            (vec!["geometry", "width"], serde_json::json!(4294967296u64)),
            (vec!["geometry", "drawable", "left"], serde_json::json!(-1)),
            (
                vec!["geometry", "drawable", "width"],
                serde_json::json!(1261),
            ),
            (
                vec!["geometry", "window", "left"],
                serde_json::json!(2147483647),
            ),
            (
                vec!["geometry", "workArea"],
                serde_json::json!({"left":0,"top":0,"width":0,"height":1}),
            ),
            (
                vec!["geometry", "workArea"],
                serde_json::json!({"left":5000,"top":0,"width":1,"height":1}),
            ),
            (vec!["geometry", "secret"], serde_json::json!("unsupported")),
        ] {
            let mut value = base.clone();
            let mut slot = &mut value;
            for field in &path {
                slot = &mut slot[*field];
            }
            *slot = bad;
            assert!(parse(&value).is_none(), "{path:?}");
        }
        assert!(Message::parse(&vec![b' '; 4097]).is_none());
        assert!(Message::parse(br#"{"abiVersion":1,"abiVersion":1}"#).is_none());
        let nan_json = serde_json::to_string(&base).unwrap().replace("3.5", "NaN");
        assert!(Message::parse(nan_json.as_bytes()).is_none());
    }
    #[test]
    fn local_display_old_stage_window_revision_cannot_revive_destroyed_snapshot() {
        let mut cache = LocalDisplay::default();
        cache.accept(parse(&sample_message(1, 1, 11, true)).unwrap());
        let g = cache.geometry;
        cache.accept(parse(&sample_message(1, 1, 11, false)).unwrap());
        cache.accept(parse(&sample_message(1, 2, 12, false)).unwrap());
        assert_eq!(cache.geometry, g);
        assert!(cache.phone);
        let invalid = serde_json::json!({"abiVersion":1,"stage":2,"revision":3,"windowId":0,"phone":true,"geometry":null,"reason":"window_destroyed"});
        cache.accept(parse(&invalid).unwrap());
        assert!(cache.geometry.is_none());
        cache.accept(parse(&sample_message(1, 4, 11, true)).unwrap());
        assert!(cache.geometry.is_none());
        cache.accept(parse(&sample_message(3, 5, 12, false)).unwrap());
        assert!(cache.geometry.is_some());
        assert!(!cache.phone);
        let mut invalid = invalid;
        invalid["stage"] = 3.into();
        invalid["revision"] = 6.into();
        invalid["reason"] = "display_unavailable".into();
        cache.accept(parse(&invalid).unwrap());
        assert!(cache.geometry.is_none());
    }
    #[test]
    fn local_display_fix1_system_work_area_rejects_partial_bounds_without_clipping() {
        for area in [
            serde_json::json!({"left":-1,"top":0,"width":1260,"height":2720}),
            serde_json::json!({"left":0,"top":-1,"width":1260,"height":2720}),
            serde_json::json!({"left":0,"top":0,"width":1261,"height":2720}),
            serde_json::json!({"left":0,"top":0,"width":1260,"height":2721}),
        ] {
            let mut value = sample_message(1, 2, 11, true);
            value["geometry"]["workArea"] = area;
            assert!(parse(&value).is_none(), "系统工作区不得clip: {value}");
        }
        let mut full = sample_message(1, 2, 11, true);
        full["geometry"]["workArea"] =
            serde_json::json!({"left":0,"top":0,"width":1260,"height":2720});
        assert_eq!(
            raw(&full).work_area,
            PixelRect {
                x: 0,
                y: 0,
                width: 1260,
                height: 2720
            }
        );
        let mut invalid_reason = sample_message(1, 3, 11, true);
        invalid_reason["reason"] = "invalid_work_area".into();
        assert!(parse(&invalid_reason).is_some());
        invalid_reason["geometry"] = serde_json::Value::Null;
        assert!(parse(&invalid_reason).is_none());
        full["reason"] = "invalid_work_area".into();
        assert!(parse(&full).is_none());
        let mut unavailable = sample_message(1, 3, 11, true);
        unavailable["reason"] = "background".into();
        assert!(parse(&unavailable).is_none());
    }
}
