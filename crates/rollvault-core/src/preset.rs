//! Folder naming: path templates, token sanitising and roll auto-increment.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_ROLL_TEMPLATE: &str = "{date}_{project}/{category}/{day}/{camera}/{roll}";
pub const DEFAULT_REPORT_TEMPLATE: &str = "{date}_{project}/03_DIT_REPORTS";

pub const TOKENS: &[&str] = &["date", "project", "category", "day", "camera", "roll", "dit"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RollMeta {
    /// Shoot date as YYYYMMDD.
    pub shoot_date: String,
    pub project_name: String,
    /// e.g. "01_FOOTAGE" or "02_AUDIO".
    pub media_category: String,
    /// e.g. "Day01".
    pub shoot_day: String,
    /// e.g. "CAM_A_FX6".
    pub camera_id: String,
    /// e.g. "A001".
    pub roll_id: String,
    pub dit_name: String,
}

impl RollMeta {
    fn token(&self, name: &str) -> Option<&str> {
        Some(match name {
            "date" => &self.shoot_date,
            "project" => &self.project_name,
            "category" => &self.media_category,
            "day" => &self.shoot_day,
            "camera" => &self.camera_id,
            "roll" => &self.roll_id,
            "dit" => &self.dit_name,
            _ => return None,
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_date(&self.shoot_date)?;
        for (label, v) in [
            (l!("Project name", "Tên dự án"), &self.project_name),
            (l!("Media category", "Phân loại media"), &self.media_category),
            (l!("Shoot day", "Ngày quay"), &self.shoot_day),
            ("Camera ID".to_string(), &self.camera_id),
            ("Roll ID".to_string(), &self.roll_id),
        ] {
            if v.trim().is_empty() {
                return Err(l!("{} cannot be empty", "{} không được để trống", label));
            }
        }
        Ok(())
    }
}

pub fn validate_date(yyyymmdd: &str) -> Result<(), String> {
    let ok = yyyymmdd.len() == 8
        && yyyymmdd.chars().all(|c| c.is_ascii_digit())
        && chrono::NaiveDate::parse_from_str(yyyymmdd, "%Y%m%d").is_ok();
    if ok {
        Ok(())
    } else {
        Err(l!("Shoot date must be a valid YYYYMMDD, got '{}'", "Ngày quay phải có dạng YYYYMMDD hợp lệ, nhận được '{}'", yyyymmdd))
    }
}

/// Make one path component safe for macOS, exFAT and SMB volumes.
pub fn sanitize_component(input: &str) -> String {
    let mut out: String = input
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c if c.is_whitespace() => '_',
            c => c,
        })
        .collect();
    // No hidden names and no "." / ".." traversal.
    while out.starts_with('.') {
        out.replace_range(0..1, "_");
    }
    // Trailing dots/spaces are invalid on exFAT/SMB.
    while out.ends_with('.') {
        out.pop();
        out.push('_');
    }
    out
}

/// Render a `/`-separated template such as `{date}_{project}/{roll}` into a
/// relative path. Every component is sanitised; empty components are errors.
pub fn render_template(template: &str, meta: &RollMeta) -> Result<PathBuf, String> {
    let template = template.trim().trim_matches('/');
    if template.is_empty() {
        return Err(l!("The path template is empty", "Mẫu đường dẫn đang trống"));
    }
    let mut path = PathBuf::new();
    for segment in template.split('/') {
        let mut rendered = String::new();
        let mut rest = segment;
        while let Some(start) = rest.find('{') {
            rendered.push_str(&rest[..start]);
            let after = &rest[start + 1..];
            let end = after
                .find('}')
                .ok_or_else(|| l!("Missing '}}' in template: {}", "Thiếu dấu '}}' trong mẫu: {}", segment))?;
            let name = &after[..end];
            let value = meta
                .token(name)
                .ok_or_else(|| l!("Unknown variable {{{}}}. Use: {}", "Biến không hợp lệ {{{}}}. Dùng: {}", name, TOKENS.join(", ")))?;
            rendered.push_str(value.trim());
            rest = &after[end + 1..];
        }
        rendered.push_str(rest);
        let clean = sanitize_component(&rendered);
        if clean.is_empty() || clean.chars().all(|c| c == '_') {
            return Err(l!("The template produces an empty folder at '{}'", "Mẫu tạo ra thư mục rỗng ở đoạn '{}'", segment));
        }
        path.push(clean);
    }
    Ok(path)
}

/// "A001" -> "A002", "B019" -> "B020", "A999" -> "A1000", "CARD7" -> "CARD8".
/// The last run of digits is incremented, keeping its zero padding.
pub fn increment_roll(roll: &str) -> String {
    let chars: Vec<char> = roll.chars().collect();
    let mut end = chars.len();
    while end > 0 && !chars[end - 1].is_ascii_digit() {
        end -= 1;
    }
    if end == 0 {
        return format!("{roll}_002");
    }
    let mut start = end;
    while start > 0 && chars[start - 1].is_ascii_digit() {
        start -= 1;
    }
    let digits: String = chars[start..end].iter().collect();
    let width = digits.len();
    let next = digits.parse::<u128>().map(|n| n + 1).unwrap_or(1);
    let prefix: String = chars[..start].iter().collect();
    let suffix: String = chars[end..].iter().collect();
    format!("{prefix}{next:0width$}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> RollMeta {
        RollMeta {
            shoot_date: "20261025".into(),
            project_name: "TVC Brand X".into(),
            media_category: "01_FOOTAGE".into(),
            shoot_day: "Day01".into(),
            camera_id: "CAM_A_FX6".into(),
            roll_id: "A001".into(),
            dit_name: "Alex".into(),
        }
    }

    #[test]
    fn renders_default_template() {
        let p = render_template(DEFAULT_ROLL_TEMPLATE, &meta()).unwrap();
        assert_eq!(p, PathBuf::from("20261025_TVC_Brand_X/01_FOOTAGE/Day01/CAM_A_FX6/A001"));
        let r = render_template(DEFAULT_REPORT_TEMPLATE, &meta()).unwrap();
        assert_eq!(r, PathBuf::from("20261025_TVC_Brand_X/03_DIT_REPORTS"));
    }

    #[test]
    fn blocks_traversal_and_bad_tokens() {
        let mut m = meta();
        m.roll_id = "..".into();
        assert!(render_template("{roll}", &m).is_err(), "'..' must be rejected");
        m.roll_id = "../../etc".into();
        let p = render_template("{roll}", &m).unwrap();
        assert_eq!(p.components().count(), 1);
        assert!(render_template("{nope}", &meta()).is_err());
        assert!(render_template("{roll", &meta()).is_err());
        assert!(render_template("", &meta()).is_err());
    }

    #[test]
    fn keeps_vietnamese_names() {
        let mut m = meta();
        m.project_name = "Phim Tết".into();
        let p = render_template("{project}", &m).unwrap();
        assert_eq!(p, PathBuf::from("Phim_Tết"));
    }

    #[test]
    fn roll_increment() {
        assert_eq!(increment_roll("A001"), "A002");
        assert_eq!(increment_roll("B019"), "B020");
        assert_eq!(increment_roll("A999"), "A1000");
        assert_eq!(increment_roll("CARD7"), "CARD8");
        assert_eq!(increment_roll("A001X"), "A002X");
        assert_eq!(increment_roll("SOUND"), "SOUND_002");
    }

    #[test]
    fn date_validation() {
        assert!(validate_date("20261025").is_ok());
        assert!(validate_date("20261325").is_err());
        assert!(validate_date("2026-10-25").is_err());
    }
}
